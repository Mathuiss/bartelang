//! `Format(value, mask)`.
//!
//! VB6's formatter had three faces and this keeps all three: the named formats
//! (`"Standard"`, `"Long Time"`), user-defined numeric masks (`"#,##0.00"`), and
//! user-defined date masks (`"yyyy-mm-dd"`).
//!
//! Date masks are told apart from numeric ones by their tokens: `y`, `d`, `h`,
//! `n` and `s` never appear in a numeric mask (which uses only `0`, `#`, `.`,
//! `,` and `%`), so any mask carrying one is treated as a date mask.  The cost is
//! that a numeric mask with literal text such as `"0.0 days"` would be misread -
//! keep literal text free of those letters.
//!
//! Note the era's trap: in a date mask `m` is *month* and `n` is *minute*.

use crate::datetime::{LocalTime, parse_timestamp};
use crate::error::BrtError;
use crate::value::{Num, Variant};

/// Formats `value` with `mask`.
pub(crate) fn apply(mask: &str, value: &Variant) -> Result<String, BrtError> {
    let trimmed = mask.trim();

    if let Some(named) = named_format(trimmed, value)? {
        return Ok(named);
    }

    if is_date_mask(trimmed) {
        let time = as_timestamp(value, trimmed)?;
        return Ok(apply_date_mask(trimmed, &time));
    }

    Ok(apply_number_mask(trimmed, value)?.to_string())
}

// ------------------------------------------------------------- named formats

fn named_format(mask: &str, value: &Variant) -> Result<Option<String>, BrtError> {
    let lowered = mask.to_ascii_lowercase();

    let masked = |section: &str| -> Result<String, BrtError> { apply_number_mask(section, value) };

    let formatted = match lowered.as_str() {
        "" | "general number" => crate::value::format_float(number_from(value)?),
        "fixed" => masked("0.00")?,
        "standard" => masked("#,##0.00")?,
        "currency" => masked("$#,##0.00")?,
        "percent" => masked("0.00%")?,
        "scientific" => scientific(number_from(value)?),
        "yes/no" => boolean_text(value, "Yes", "No"),
        "true/false" => boolean_text(value, "True", "False"),
        "on/off" => boolean_text(value, "On", "Off"),
        "short date" => apply_date_mask("yyyy-mm-dd", &as_timestamp(value, mask)?),
        "long date" => apply_date_mask("dddd, mmmm d, yyyy", &as_timestamp(value, mask)?),
        "short time" => apply_date_mask("hh:nn", &as_timestamp(value, mask)?),
        "long time" => apply_date_mask("hh:nn:ss", &as_timestamp(value, mask)?),
        _ => return Ok(None),
    };
    Ok(Some(formatted))
}

fn number_from(value: &Variant) -> Result<f64, BrtError> {
    Ok(match value.to_num()? {
        Num::Int(v) => v as f64,
        Num::Float(v) => v,
    })
}

fn boolean_text(value: &Variant, when_true: &str, when_false: &str) -> String {
    if value.truthy() {
        when_true.to_string()
    } else {
        when_false.to_string()
    }
}

/// `1.23E+04`, the shape VB6 produced.
fn scientific(value: f64) -> String {
    if value == 0.0 {
        return "0.00E+00".to_string();
    }
    let mut exponent = value.abs().log10().floor() as i32;
    let mut mantissa = value / 10f64.powi(exponent);
    // Guard against log10 landing just below an exact power of ten.
    if mantissa.abs() >= 10.0 {
        mantissa /= 10.0;
        exponent += 1;
    } else if mantissa.abs() < 1.0 {
        mantissa *= 10.0;
        exponent -= 1;
    }
    format!(
        "{mantissa:.2}E{}{:02}",
        if exponent < 0 { '-' } else { '+' },
        exponent.abs()
    )
}

// ------------------------------------------------------------ numeric masks

/// Splits a mask section into the literal text before and after the digit
/// placeholders.  `None` means the section has no placeholders at all, in which
/// case it is pure literal text.
fn split_section(section: &str) -> (String, Option<String>, String) {
    let body: Vec<char> = section.chars().filter(|c| *c != '%').collect();
    let first = body.iter().position(|c| *c == '0' || *c == '#');
    let last = body.iter().rposition(|c| *c == '0' || *c == '#');
    match (first, last) {
        (Some(start), Some(end)) => (
            body[..start].iter().collect(),
            Some(body[start..=end].iter().collect()),
            body[end + 1..].iter().collect(),
        ),
        _ => (body.iter().collect(), None, String::new()),
    }
}

struct Style {
    min_integer: usize,
    min_decimals: usize,
    max_decimals: usize,
    grouped: bool,
}

fn read_placeholders(placeholders: &str) -> Style {
    let (integer, fraction) = match placeholders.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (placeholders, None),
    };
    Style {
        min_integer: integer.chars().filter(|c| *c == '0').count(),
        min_decimals: fraction
            .map(|text| text.chars().filter(|c| *c == '0').count())
            .unwrap_or(0),
        max_decimals: fraction
            .map(|text| text.chars().filter(|c| matches!(c, '0' | '#')).count())
            .unwrap_or(0),
        grouped: integer.contains(','),
    }
}

fn apply_number_mask(mask: &str, value: &Variant) -> Result<String, BrtError> {
    let number = match value.to_num()? {
        Num::Int(v) => v as f64,
        Num::Float(v) => v,
    };
    let sections: Vec<&str> = mask.split(';').collect();
    let positive = sections.first().copied().unwrap_or("");
    let negative = sections.get(1).copied();
    let zero = sections.get(2).copied();

    let (section, negative_number) = if number < 0.0 {
        match negative {
            Some(text) => (text, false),
            None => (positive, true),
        }
    } else if number == 0.0 {
        (zero.unwrap_or(positive), false)
    } else {
        (positive, false)
    };

    Ok(render_section(section, number.abs(), negative_number))
}

fn render_section(section: &str, magnitude: f64, negative: bool) -> String {
    let percent = section.contains('%');
    let (prefix, placeholders, suffix) = split_section(section);
    let Some(placeholders) = placeholders else {
        // No digit placeholders: the section is literal text.
        return prefix;
    };

    let style = read_placeholders(&placeholders);
    let scaled = if percent { magnitude * 100.0 } else { magnitude };

    let mut out = prefix;
    if negative {
        out.push('-');
    }
    out.push_str(&render_number(scaled, &style));
    out.push_str(&suffix);
    if percent {
        out.push('%');
    }
    out
}

fn render_number(value: f64, style: &Style) -> String {
    let mut text = format!("{:.*}", style.max_decimals, value);
    if style.max_decimals > style.min_decimals {
        if let Some((integer, fraction)) = text.split_once('.') {
            let mut trimmed = fraction.trim_end_matches('0').to_string();
            while trimmed.chars().count() < style.min_decimals {
                trimmed.push('0');
            }
            text = if trimmed.is_empty() {
                integer.to_string()
            } else {
                format!("{integer}.{trimmed}")
            };
        }
    }

    let (mut integer, fraction) = match text.split_once('.') {
        Some((integer, fraction)) => (integer.to_string(), Some(fraction.to_string())),
        None => (text, None),
    };
    while integer.chars().count() < style.min_integer {
        integer.insert(0, '0');
    }
    if style.grouped {
        integer = group_digits(&integer);
    }

    match fraction {
        Some(fraction) => format!("{integer}.{fraction}"),
        None => integer,
    }
}

fn group_digits(digits: &str) -> String {
    let sign_at = digits.starts_with('-');
    let body = if sign_at { &digits[1..] } else { digits };
    let mut out = String::new();
    for (index, ch) in body.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    let grouped: String = out.chars().rev().collect();
    if sign_at {
        format!("-{grouped}")
    } else {
        grouped
    }
}

// --------------------------------------------------------------- date masks

/// True when the mask carries a date or time token.
fn is_date_mask(mask: &str) -> bool {
    let lowered = mask.to_ascii_lowercase();
    if lowered.contains("am/pm") || lowered.contains("a/p") {
        return true;
    }
    lowered
        .chars()
        .any(|c| matches!(c, 'y' | 'd' | 'h' | 'n' | 's'))
}

fn as_timestamp(value: &Variant, mask: &str) -> Result<LocalTime, BrtError> {
    match value {
        Variant::String(text) => parse_timestamp(text).ok_or_else(|| {
            BrtError::invalid_call(format!(
                "Format: {text:?} is not a timestamp, so the mask {mask:?} cannot apply"
            ))
        }),
        other => Err(BrtError::invalid_call(format!(
            "Format: a date mask needs a timestamp, not {}",
            other.type_name()
        ))),
    }
}

fn apply_date_mask(mask: &str, time: &LocalTime) -> String {
    let lowered = mask.to_ascii_lowercase();
    let twelve_hour = is_twelve_hour(&lowered);
    let original: Vec<char> = mask.chars().collect();
    let lowered: Vec<char> = lowered.chars().collect();

    let mut out = String::new();
    let mut index = 0;
    while index < lowered.len() {
        match date_token(&lowered, index, time, twelve_hour) {
            Some((text, consumed)) => {
                out.push_str(&text);
                index += consumed;
            }
            None => {
                out.push(original[index]);
                index += 1;
            }
        }
    }
    out
}

/// `AM/PM` or `A/P` in the mask switches `h`/`hh` to a 12-hour clock, which is
/// the rule VB6 documented.
fn is_twelve_hour(lowered_mask: &str) -> bool {
    lowered_mask.contains("am/pm") || lowered_mask.contains("a/p")
}

/// Longest token first, so `yyyy` is not read as `yy` plus a literal.
fn date_token(
    mask: &[char],
    position: usize,
    time: &LocalTime,
    twelve_hour: bool,
) -> Option<(String, usize)> {
    let rest: String = mask[position..].iter().collect();
    let month_name = time.month_name();
    let day_name = time.day_name();
    let hour = if twelve_hour {
        let on_a_clock = time.hour % 12;
        if on_a_clock == 0 { 12 } else { on_a_clock }
    } else {
        time.hour
    };

    let table: [(&str, String); 20] = [
        ("yyyy", format!("{:04}", time.year)),
        ("yy", format!("{:02}", time.year.rem_euclid(100))),
        ("mmmm", month_name.to_string()),
        ("mmm", month_name[..3].to_string()),
        ("mm", format!("{:02}", time.month)),
        ("dddd", day_name.to_string()),
        ("ddd", day_name[..3].to_string()),
        ("dd", format!("{:02}", time.day)),
        ("hh", format!("{hour:02}")),
        ("nn", format!("{:02}", time.minute)),
        ("ss", format!("{:02}", time.second)),
        (
            "am/pm",
            if time.hour < 12 { "AM" } else { "PM" }.to_string(),
        ),
        ("a/p", if time.hour < 12 { "A" } else { "P" }.to_string()),
        ("m", time.month.to_string()),
        ("d", time.day.to_string()),
        ("h", hour.to_string()),
        ("n", time.minute.to_string()),
        ("s", time.second.to_string()),
        ("tttt", time.time()),
        ("tt", if time.hour < 12 { "AM" } else { "PM" }.to_string()),
    ];

    table
        .iter()
        .find(|(token, _)| rest.starts_with(*token))
        .map(|(token, text)| (text.clone(), token.chars().count()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(mask: &str, value: Variant) -> String {
        apply(mask, &value).unwrap()
    }

    fn number(value: f64) -> Variant {
        Variant::Float(value)
    }

    #[test]
    fn named_numeric_formats() {
        assert_eq!(text("General Number", number(1234.5)), "1234.5");
        assert_eq!(text("Fixed", number(5.0)), "5.00");
        assert_eq!(text("Standard", number(1234567.891)), "1,234,567.89");
        assert_eq!(text("Currency", number(1234.5)), "$1,234.50");
        assert_eq!(text("Percent", number(0.5)), "50.00%");
        assert_eq!(text("Scientific", number(12345.0)), "1.23E+04");
        assert_eq!(text("Scientific", number(0.005)), "5.00E-03");
        assert_eq!(text("Scientific", number(0.0)), "0.00E+00");
    }

    #[test]
    fn named_boolean_formats() {
        assert_eq!(text("Yes/No", Variant::Bool(true)), "Yes");
        assert_eq!(text("Yes/No", Variant::Int(0)), "No");
        assert_eq!(text("On/Off", Variant::Bool(true)), "On");
        assert_eq!(text("True/False", Variant::Int(0)), "False");
    }

    #[test]
    fn user_defined_numeric_masks() {
        assert_eq!(text("#,##0.00", number(1234567.891)), "1,234,567.89");
        assert_eq!(text("0", number(7.6)), "8");
        assert_eq!(text("0.000", number(7.6)), "7.600");
        assert_eq!(text("#.##", number(7.6)), "7.6");
        assert_eq!(text("000", Variant::Int(7)), "007");
        assert_eq!(text("0%", number(0.25)), "25%");
        assert_eq!(text("$#,##0.00", number(9.5)), "$9.50");
    }

    #[test]
    fn negative_sections_control_the_sign() {
        // No negative section: a minus sign is added after any literal prefix.
        assert_eq!(text("$0.00", number(-3.5)), "$-3.50");
        // With one, the section decides the whole shape.
        assert_eq!(text("0.00;(0.00)", number(-3.5)), "(3.50)");
        assert_eq!(text("0.00;(0.00)", number(3.5)), "3.50");
        // A third section handles zero.
        assert_eq!(text("0;-0;\"zero\"", number(0.0)), "\"zero\"");
    }

    #[test]
    fn date_masks_use_n_for_minutes() {
        let stamp = Variant::string("2026-09-24 16:01:16");
        assert_eq!(text("yyyy-mm-dd", stamp.clone()), "2026-09-24");
        assert_eq!(text("hh:nn:ss", stamp.clone()), "16:01:16");
        assert_eq!(text("yy/m/d", stamp.clone()), "26/9/24");
        assert_eq!(text("dddd, mmmm d, yyyy", stamp.clone()), "Thursday, September 24, 2026");
        assert_eq!(text("ddd mmm", stamp.clone()), "Thu Sep");
        assert_eq!(text("h:nn AM/PM", stamp.clone()), "4:01 PM");
        assert_eq!(text("yyyy-mm-dd", Variant::string("2026-09-24")), "2026-09-24");
    }

    #[test]
    fn named_date_formats() {
        let stamp = Variant::string("2026-09-24 16:01:16");
        assert_eq!(text("Short Date", stamp.clone()), "2026-09-24");
        assert_eq!(text("Long Date", stamp.clone()), "Thursday, September 24, 2026");
        assert_eq!(text("Short Time", stamp.clone()), "16:01");
        assert_eq!(text("Long Time", stamp.clone()), "16:01:16");
    }

    #[test]
    fn a_date_mask_on_something_that_is_not_a_timestamp_is_reported() {
        let error = apply("yyyy-mm-dd", &Variant::Int(5)).unwrap_err();
        assert_eq!(error.number, 5);
        assert!(error.message.contains("date mask"));

        let error = apply("yyyy", &Variant::string("nonsense")).unwrap_err();
        assert!(error.message.contains("not a timestamp"));
    }

    #[test]
    fn a_mask_without_placeholders_is_literal_text() {
        assert_eq!(text("abc", Variant::Int(5)), "abc");
    }

    #[test]
    fn the_date_mask_heuristic_has_a_documented_cost() {
        // "n" is a minute token, so a literal mask containing one is mistaken
        // for a date mask.  This is the trade the module documents.
        let error = apply("n/a", &Variant::Int(5)).unwrap_err();
        assert!(error.message.contains("date mask"), "{}", error.message);
    }
}
