//! `VBScript.RegExp`: the text-munging partner to `Split`.

use regex::RegexBuilder;

use crate::error::BrtError;
use crate::objects::{BartObject, Member};
use crate::value::Variant;

/// `CreateObject("VBScript.RegExp")`
///
/// The pattern is compiled on each use rather than cached, so that assigning
/// `Pattern` and then matching behaves the way a script expects.
#[derive(Default)]
pub struct RegExp {
    pattern: String,
    global: bool,
    ignore_case: bool,
}

impl RegExp {
    /// An unconfigured expression; set [`RegExp`]'s `Pattern` before using it.
    pub fn new() -> Self {
        RegExp::default()
    }

    fn compile(&self) -> Result<regex::Regex, BrtError> {
        RegexBuilder::new(&self.pattern)
            .case_insensitive(self.ignore_case)
            .build()
            .map_err(|e| {
                BrtError::new(
                    5,
                    format!("Invalid regular expression {:?}: {e}", self.pattern),
                )
            })
    }
}

impl BartObject for RegExp {
    fn type_name(&self) -> String {
        "VBScript.RegExp".to_string()
    }

    fn member_kind(&self, name: &str) -> Member {
        match name.to_ascii_lowercase().as_str() {
            "test" | "replace" => Member::Method,
            "pattern" | "global" | "ignorecase" => Member::Property,
            _ => Member::Unknown,
        }
    }

    fn call_method(&mut self, method: &str, args: Vec<Variant>) -> Result<Variant, BrtError> {
        match method.to_ascii_lowercase().as_str() {
            "test" => {
                if args.len() != 1 {
                    return Err(BrtError::wrong_arg_count("RegExp.Test", args.len(), "1"));
                }
                let text = args[0].as_string()?;
                Ok(Variant::Bool(self.compile()?.is_match(&text)))
            }
            "replace" => {
                if args.len() != 2 {
                    return Err(BrtError::wrong_arg_count("RegExp.Replace", args.len(), "2"));
                }
                let text = args[0].as_string()?;
                let replacement = args[1].as_string()?;
                let regex = self.compile()?;
                // `Global` decides between every match and just the first, as
                // it did in VBScript.
                let replaced = if self.global {
                    regex.replace_all(&text, replacement.as_str()).into_owned()
                } else {
                    regex.replace(&text, replacement.as_str()).into_owned()
                };
                Ok(Variant::String(replaced))
            }
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }

    fn get_property(&self, prop_name: &str) -> Result<Variant, BrtError> {
        match prop_name.to_ascii_lowercase().as_str() {
            "pattern" => Ok(Variant::String(self.pattern.clone())),
            "global" => Ok(Variant::Bool(self.global)),
            "ignorecase" => Ok(Variant::Bool(self.ignore_case)),
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }

    /// `re.Pattern = "..."`, `re.Global = True`, `re.IgnoreCase = True`
    fn set_property(&mut self, prop_name: &str, value: Variant) -> Result<(), BrtError> {
        match prop_name.to_ascii_lowercase().as_str() {
            "pattern" => {
                self.pattern = value.as_string()?;
                Ok(())
            }
            "global" => {
                self.global = value.truthy();
                Ok(())
            }
            "ignorecase" => {
                self.ignore_case = value.truthy();
                Ok(())
            }
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regex(pattern: &str) -> RegExp {
        let mut regex = RegExp::new();
        regex.set_property("pattern", Variant::string(pattern)).unwrap();
        regex
    }

    #[test]
    fn test_reports_a_match() {
        let mut regex = regex("^h.llo$");
        assert!(regex
            .call_method("test", vec![Variant::string("hello")])
            .unwrap()
            .truthy());
        assert!(!regex
            .call_method("test", vec![Variant::string("hellos")])
            .unwrap()
            .truthy());
    }

    #[test]
    fn ignore_case_is_off_by_default_and_can_be_turned_on() {
        let mut regex = regex("hello");
        assert!(!regex
            .call_method("test", vec![Variant::string("HELLO")])
            .unwrap()
            .truthy());
        regex.set_property("ignorecase", Variant::Bool(true)).unwrap();
        assert!(regex
            .call_method("test", vec![Variant::string("HELLO")])
            .unwrap()
            .truthy());
    }

    #[test]
    fn replace_respects_global() {
        let mut regex = regex("a");
        assert_eq!(
            regex
                .call_method(
                    "replace",
                    vec![Variant::string("banana"), Variant::string("-")]
                )
                .unwrap()
                .display_string(),
            "b-nana"
        );
        regex.set_property("global", Variant::Bool(true)).unwrap();
        assert_eq!(
            regex
                .call_method(
                    "replace",
                    vec![Variant::string("banana"), Variant::string("-")]
                )
                .unwrap()
                .display_string(),
            "b-n-n-"
        );
    }

    #[test]
    fn capture_groups_can_be_referenced_in_a_replacement() {
        let mut regex = regex(r"(\w+), (\w+)");
        regex.set_property("global", Variant::Bool(true)).unwrap();
        assert_eq!(
            regex
                .call_method(
                    "replace",
                    vec![Variant::string("Doe, John"), Variant::string("$2 $1")]
                )
                .unwrap()
                .display_string(),
            "John Doe"
        );
    }

    #[test]
    fn a_bad_pattern_is_reported_when_it_is_used() {
        let mut regex = regex("(unclosed");
        let error = regex
            .call_method("test", vec![Variant::string("x")])
            .unwrap_err();
        assert_eq!(error.number, 5);
        assert!(error.message.contains("Invalid regular expression"));
    }

    #[test]
    fn read_only_properties_report_438_when_assigned() {
        let mut regex = regex("a");
        let error = regex
            .set_property("nope", Variant::Int(1))
            .unwrap_err();
        assert_eq!(error.number, 438);
    }
}
