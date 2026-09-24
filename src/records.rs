//! The record format behind `Write #n` and `Input #n`.
//!
//! VB6's `Write` produced one line per record, with values comma-separated and
//! strings quoted, and `Input` parsed exactly that back.  It was how period code
//! persisted structured data before it reached for a database, and the pair is
//! the missing half of the vintage file channels.

use crate::error::BrtError;
use crate::value::Variant;

/// Encodes one record.  The result is the line's contents, without the newline.
pub(crate) fn encode(values: &[Variant]) -> Result<String, BrtError> {
    let mut fields = Vec::with_capacity(values.len());
    for value in values {
        fields.push(encode_value(value)?);
    }
    Ok(fields.join(","))
}

fn encode_value(value: &Variant) -> Result<String, BrtError> {
    Ok(match value {
        Variant::String(text) => {
            // VB6 doubled an embedded quote, as CSV dialects do.
            format!("\"{}\"", text.replace('"', "\"\""))
        }
        Variant::Bool(true) => "#TRUE#".to_string(),
        Variant::Bool(false) => "#FALSE#".to_string(),
        Variant::Int(value) => value.to_string(),
        Variant::Float(value) => crate::value::format_float(*value),
        // VB6 wrote nothing at all for Empty.
        Variant::Empty => String::new(),
        Variant::Null => "#NULL#".to_string(),
        other => {
            return Err(BrtError::type_mismatch(format!(
                "Write cannot record a {}",
                other.type_name()
            )))
        }
    })
}

/// Parses one record back into its fields.
pub(crate) fn parse(line: &str) -> Vec<Variant> {
    let chars: Vec<char> = line.chars().collect();
    let mut fields = Vec::new();
    let mut index = 0;

    while index <= chars.len() {
        // Leading spaces are not part of a field.
        while index < chars.len() && chars[index] == ' ' {
            index += 1;
        }

        if index < chars.len() && chars[index] == '"' {
            index += 1;
            let mut text = String::new();
            while index < chars.len() {
                if chars[index] == '"' {
                    if index + 1 < chars.len() && chars[index + 1] == '"' {
                        text.push('"');
                        index += 2;
                        continue;
                    }
                    index += 1;
                    break;
                }
                text.push(chars[index]);
                index += 1;
            }
            fields.push(Variant::String(text));
        } else {
            let mut text = String::new();
            while index < chars.len() && chars[index] != ',' {
                text.push(chars[index]);
                index += 1;
            }
            let trimmed = text.trim();
            fields.push(if trimmed.is_empty() {
                Variant::Empty
            } else {
                scalar(trimmed)
            });
        }

        while index < chars.len() && chars[index] == ' ' {
            index += 1;
        }
        if index < chars.len() && chars[index] == ',' {
            index += 1;
            continue;
        }
        break;
    }

    fields
}

/// A bare field: the numeric and boolean literals `Write` emits, or failing
/// that the text itself.
fn scalar(text: &str) -> Variant {
    match text {
        "#TRUE#" => return Variant::Bool(true),
        "#FALSE#" => return Variant::Bool(false),
        "#NULL#" => return Variant::Null,
        _ => {}
    }
    if let Ok(value) = text.parse::<i64>() {
        return Variant::Int(value);
    }
    if let Ok(value) = text.parse::<f64>() {
        return Variant::Float(value);
    }
    Variant::String(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips() {
        let values = vec![
            Variant::string("alpha"),
            Variant::Int(42),
            Variant::Float(1.5),
            Variant::Bool(true),
            Variant::Bool(false),
            Variant::Empty,
        ];
        let line = encode(&values).unwrap();
        assert_eq!(line, "\"alpha\",42,1.5,#TRUE#,#FALSE#,");

        let parsed = parse(&line);
        assert_eq!(parsed.len(), 6);
        assert_eq!(parsed[0].display_string(), "alpha");
        assert_eq!(parsed[1].to_int().unwrap(), 42);
        assert_eq!(parsed[2].to_float().unwrap(), 1.5);
        assert!(parsed[3].truthy());
        assert!(!parsed[4].truthy());
        assert!(matches!(parsed[5], Variant::Empty));
    }

    #[test]
    fn a_comma_inside_a_quoted_string_is_not_a_separator() {
        let line = encode(&[Variant::string("a,b"), Variant::Int(1)]).unwrap();
        assert_eq!(line, "\"a,b\",1");
        let parsed = parse(&line);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].display_string(), "a,b");
        assert_eq!(parsed[1].to_int().unwrap(), 1);
    }

    #[test]
    fn an_embedded_quote_survives_the_round_trip() {
        let line = encode(&[Variant::string("say \"hi\"")]).unwrap();
        assert_eq!(line, "\"say \"\"hi\"\"\"");
        assert_eq!(parse(&line)[0].display_string(), "say \"hi\"");
    }

    #[test]
    fn spaces_around_fields_are_ignored() {
        let parsed = parse("  \"a b\" , 7 , x ");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].display_string(), "a b");
        assert_eq!(parsed[1].to_int().unwrap(), 7);
        assert_eq!(parsed[2].display_string(), "x");
    }

    #[test]
    fn null_is_encoded_and_decoded() {
        let line = encode(&[Variant::Null]).unwrap();
        assert_eq!(line, "#NULL#");
        assert!(matches!(parse(&line)[0], Variant::Null));
    }

    #[test]
    fn objects_and_arrays_refuse_to_be_recorded() {
        let array = Variant::Array(std::rc::Rc::new(std::cell::RefCell::new(vec![
            Variant::Int(1),
        ])));
        let error = encode(&[array]).unwrap_err();
        assert_eq!(error.number, 13);
    }

    #[test]
    fn an_empty_line_yields_one_empty_field() {
        assert_eq!(parse("").len(), 1);
        assert!(matches!(parse("")[0], Variant::Empty));
    }
}
