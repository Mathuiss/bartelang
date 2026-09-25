//! Platform objects.
//!
//! `CreateObject("HTTP")` yields a real Rust value that answers `http.Open`,
//! `http.Send` and `http.ResponseText`.  Behind the curtain the HTTP work is
//! done by a safe `reqwest` client.
//!
//! A name is a plain key into the table below - there is no COM, no registry and
//! nothing Windows-specific about it.  The names follow the era's convention
//! (`Scripting.FileSystemObject`, `VBScript.RegExp`, ...) because that is what
//! period code says, not because anything Microsoft is involved.

pub mod collections;
pub mod filesystem;
pub mod process;
pub mod regex;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crate::error::BrtError;
use crate::value::Variant;

use collections::{Collection, Dictionary};
use filesystem::FileSystemObject;
use process::WshShell;
use regex::RegExp;

/// What kind of member a name refers to, so the interpreter can dispatch
/// deterministically instead of guessing between method and property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Member {
    Method,
    Property,
    Unknown,
}

/// The interface every Bartelang object implements.
pub trait BartObject {
    /// Invokes a method.  Implementations reject names they do not know with
    /// error 438.
    fn call_method(&mut self, method_name: &str, args: Vec<Variant>) -> Result<Variant, BrtError>;
    /// Reads a property.
    fn get_property(&self, prop_name: &str) -> Result<Variant, BrtError>;

    /// Defaults to `Unknown` so implementations may rely on the lenient
    /// fallback in the interpreter.
    fn member_kind(&self, _name: &str) -> Member {
        Member::Unknown
    }

    fn type_name(&self) -> String {
        "Object".to_string()
    }

    /// Assignment to a member: `obj.Prop = value`.  Defaults to an error, so
    /// read-only objects need not think about it.
    fn set_property(&mut self, prop_name: &str, _value: Variant) -> Result<(), BrtError> {
        Err(BrtError::no_such_member(&self.type_name(), prop_name))
    }

    /// Assignment to the default member: `obj(key) = value`.
    fn set_item(&mut self, _key: Variant, _value: Variant) -> Result<(), BrtError> {
        Err(BrtError::no_such_member(&self.type_name(), "Item"))
    }
}

/// Every object type `CreateObject` can build: the canonical name, and the terse
/// aliases that mean the same thing.  Every lookup is case-insensitive.
///
/// This is the single source of truth.  `build_object` handles the canonical
/// names, the error message that lists what exists is generated from this table,
/// and `every_advertised_name_and_alias_resolves` keeps the two in step.
pub const OBJECT_TYPES: &[(&str, &[&str])] = &[
    // HTTP was reduced to a plain mnemonic by an earlier decision, so it is
    // already the short name and has no alias to offer.
    ("HTTP", &[]),
    ("Scripting.FileSystemObject", &["FS"]),
    ("Scripting.Dictionary", &["DICT"]),
    // `COLL` follows the abbreviation pattern the others set; `LIST` is what the
    // thing acts like, and it catches anyone who types `COL`.
    ("Collection", &["COLL", "LIST"]),
    ("VBScript.RegExp", &["REGEX"]),
    ("WScript.Shell", &["PROC"]),
];

fn build_object(canonical: &str) -> Option<Variant> {
    let object: Rc<RefCell<dyn BartObject>> = match canonical {
        "HTTP" => Rc::new(RefCell::new(HttpClient::new())),
        "Scripting.FileSystemObject" => Rc::new(RefCell::new(FileSystemObject::new())),
        "Scripting.Dictionary" => Rc::new(RefCell::new(Dictionary::new())),
        "Collection" => Rc::new(RefCell::new(Collection::new())),
        "VBScript.RegExp" => Rc::new(RefCell::new(RegExp::new())),
        "WScript.Shell" => Rc::new(RefCell::new(WshShell::new())),
        _ => return None,
    };
    Some(Variant::Object(object))
}

/// The `CreateObject` factory.  Canonical names and aliases are matched
/// case-insensitively, like every other identifier in the language.
pub fn create_object(type_name: &str) -> Result<Variant, BrtError> {
    let wanted = type_name.trim();
    let lowered = wanted.to_ascii_lowercase();

    let canonical = OBJECT_TYPES
        .iter()
        .find(|(canonical, aliases)| {
            canonical.to_ascii_lowercase() == lowered
                || aliases
                    .iter()
                    .any(|alias| alias.to_ascii_lowercase() == lowered)
        })
        .map(|(canonical, _)| *canonical);

    match canonical.and_then(build_object) {
        Some(value) => Ok(value),
        None => Err(BrtError::cant_create_object(wanted, OBJECT_TYPES)),
    }
}

/// The HTTP client behind `CreateObject("HTTP")`.
pub struct HttpClient {
    method: String,
    url: String,
    is_async: bool,
    body: Option<String>,
    headers: Vec<(String, String)>,
    response_text: String,
    status: i64,
    status_text: String,
    ready_state: i64,
    timeout: Duration,
    opened: bool,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient {
    /// A client with no request configured yet.
    pub fn new() -> Self {
        HttpClient {
            method: "GET".to_string(),
            url: String::new(),
            is_async: false,
            body: None,
            headers: Vec::new(),
            response_text: String::new(),
            status: 0,
            status_text: String::new(),
            ready_state: 0,
            timeout: Duration::from_secs(30),
            opened: false,
        }
    }

    fn perform_send(&mut self) -> Result<Variant, BrtError> {
        if !self.opened {
            return Err(BrtError::invalid_call(
                "HTTP.Send called before Open",
            ));
        }
        if self.url.trim().is_empty() {
            return Err(BrtError::invalid_call("HTTP.Open was given an empty URL"));
        }

        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| BrtError::http(format!("Could not build HTTP client: {e}")))?;

        let method = reqwest::Method::from_bytes(self.method.trim().to_uppercase().as_bytes())
            .map_err(|_| BrtError::http(format!("Invalid HTTP method: {}", self.method)))?;

        let mut request = client.request(method, self.url.trim());
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(body) = &self.body {
            request = request.body(body.clone());
        }

        let response = request
            .send()
            .map_err(|e| BrtError::http(format!("HTTP request failed for {}: {e}", self.url)))?;

        self.status = response.status().as_u16() as i64;
        self.status_text = response
            .status()
            .canonical_reason()
            .unwrap_or("")
            .to_string();
        self.response_text = response
            .text()
            .map_err(|e| BrtError::http(format!("Could not read response body: {e}")))?;
        self.ready_state = 4;
        Ok(Variant::Empty)
    }
}

impl BartObject for HttpClient {
    fn type_name(&self) -> String {
        "HTTP".to_string()
    }

    fn member_kind(&self, name: &str) -> Member {
        match name.to_ascii_lowercase().as_str() {
            "open" | "send" | "setrequestheader" | "settimeouts" | "abort" | "getallresponseheaders" => {
                Member::Method
            }
            "responsetext" | "responsebody" | "status" | "statustext" | "readystate" => {
                Member::Property
            }
            _ => Member::Unknown,
        }
    }

    fn call_method(&mut self, method_name: &str, args: Vec<Variant>) -> Result<Variant, BrtError> {
        match method_name.to_ascii_lowercase().as_str() {
            "open" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(BrtError::wrong_arg_count("HTTP.Open", args.len(), "2 or 3"));
                }
                self.method = args[0].as_string()?;
                self.url = args[1].as_string()?;
                if let Some(async_flag) = args.get(2) {
                    self.is_async = async_flag.truthy();
                }
                self.opened = true;
                self.ready_state = 1;
                Ok(Variant::Empty)
            }
            "setrequestheader" => {
                if args.len() != 2 {
                    return Err(BrtError::wrong_arg_count(
                        "HTTP.SetRequestHeader",
                        args.len(),
                        "2",
                    ));
                }
                self.headers
                    .push((args[0].as_string()?, args[1].as_string()?));
                Ok(Variant::Empty)
            }
            "settimeouts" => {
                if args.is_empty() || args.len() > 4 {
                    return Err(BrtError::wrong_arg_count(
                        "HTTP.SetTimeouts",
                        args.len(),
                        "1 to 4",
                    ));
                }
                // The 4th argument is the receive timeout, in milliseconds.
                let millis = args[args.len() - 1].to_int()?.max(1) as u64;
                self.timeout = Duration::from_millis(millis);
                Ok(Variant::Empty)
            }
            "send" => {
                if args.len() > 1 {
                    return Err(BrtError::wrong_arg_count("HTTP.Send", args.len(), "0 or 1"));
                }
                self.body = match args.into_iter().next() {
                    Some(Variant::Empty) | None => None,
                    Some(value) => Some(value.as_string()?),
                };
                self.perform_send()
            }
            "abort" => {
                self.ready_state = 0;
                Ok(Variant::Empty)
            }
            "getallresponseheaders" => Ok(Variant::String(String::new())),
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }

    fn get_property(&self, prop_name: &str) -> Result<Variant, BrtError> {
        match prop_name.to_ascii_lowercase().as_str() {
            "responsetext" => Ok(Variant::String(self.response_text.clone())),
            "responsebody" => Ok(Variant::String(self.response_text.clone())),
            "status" => Ok(Variant::Int(self.status)),
            "statustext" => Ok(Variant::String(self.status_text.clone())),
            "readystate" => Ok(Variant::Int(self.ready_state)),
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }
}

/// The implicit `Err` object available inside `Catch` blocks.
pub struct ErrObject {
    pub number: i64,
    pub description: String,
    pub source: String,
    /// The source line the error came from, or 0 when the interpreter did not
    /// know - which is what VB6's `Erl` reported outside a handler.
    pub line: i64,
    /// The file the error came from, as the loader names it (for example
    /// `lib/math.btm`), or empty when the program was not loaded from files.
    pub file: String,
}

impl Default for ErrObject {
    fn default() -> Self {
        Self::new()
    }
}

impl ErrObject {
    /// An empty `Err`, as it looks before anything has gone wrong.
    pub fn new() -> Self {
        ErrObject {
            number: 0,
            description: String::new(),
            source: String::new(),
            line: 0,
            file: String::new(),
        }
    }

    /// Fills the object in from a failed statement.  `file` is the name of the
    /// source file the failing statement came from, when there is one.
    pub fn set_from(&mut self, error: &BrtError, file: Option<&str>) {
        self.number = error.number as i64;
        self.description = error.message.clone();
        self.source = error
            .source
            .clone()
            .unwrap_or_else(|| "Bartelang".to_string());
        self.line = error.line.unwrap_or(0) as i64;
        self.file = file.unwrap_or("").to_string();
    }

    /// Resets the object, as `Err.Clear` does.
    pub fn clear(&mut self) {
        self.number = 0;
        self.description.clear();
        self.source.clear();
        self.line = 0;
        self.file.clear();
    }
}

impl BartObject for ErrObject {
    fn type_name(&self) -> String {
        "Err".to_string()
    }

    fn member_kind(&self, name: &str) -> Member {
        match name.to_ascii_lowercase().as_str() {
            "clear" | "raise" => Member::Method,
            "number" | "description" | "message" | "source" | "line" | "file" => {
                Member::Property
            }
            _ => Member::Unknown,
        }
    }

    fn call_method(&mut self, method_name: &str, args: Vec<Variant>) -> Result<Variant, BrtError> {
        match method_name.to_ascii_lowercase().as_str() {
            "clear" => {
                if !args.is_empty() {
                    return Err(BrtError::wrong_arg_count("Err.Clear", args.len(), "0"));
                }
                self.clear();
                Ok(Variant::Empty)
            }
            // `Err.Raise number, source, description` - how user code signals
            // its own failure, so `Catch` is useful for libraries and not just
            // for errors the interpreter happens to produce.
            "raise" => {
                if args.is_empty() || args.len() > 3 {
                    return Err(BrtError::wrong_arg_count("Err.Raise", args.len(), "1 to 3"));
                }
                let number = args[0].to_int()?;
                if number < 1 || number > i32::MAX as i64 {
                    return Err(BrtError::invalid_call(
                        "Err.Raise number must be between 1 and 2147483647",
                    ));
                }
                let source = args
                    .get(1)
                    .map(|value| value.display_string())
                    .filter(|text| !text.is_empty());
                let description = args
                    .get(2)
                    .map(|value| value.display_string())
                    .filter(|text| !text.is_empty());
                let message = description
                    .or_else(|| source.clone())
                    .unwrap_or_else(|| "Application-defined error".to_string());
                Err(BrtError::new(number as i32, message).with_source(source))
            }
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }

    fn get_property(&self, prop_name: &str) -> Result<Variant, BrtError> {
        match prop_name.to_ascii_lowercase().as_str() {
            "number" => Ok(Variant::Int(self.number)),
            // The design document uses `Err.Message`; VB6 calls it
            // `Err.Description`.  Both spellings answer.
            "description" | "message" => Ok(Variant::String(self.description.clone())),
            "source" => Ok(Variant::String(self.source.clone())),
            "line" => Ok(Variant::Int(self.line)),
            // Bartelang addition: VB6's `Err` could not say which file failed,
            // which stops mattering the moment a program has more than one.
            "file" => Ok(Variant::String(self.file.clone())),
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(v: &Variant) -> &Rc<RefCell<dyn BartObject>> {
        match v {
            Variant::Object(o) => o,
            other => panic!("expected an object, got {other:?}"),
        }
    }

    /// What the object calls itself - the canonical name, whatever alias built
    /// it.
    fn type_name_of(v: &Variant) -> String {
        object(v).borrow().type_name()
    }

    #[test]
    fn the_canonical_name_resolves_case_insensitively() {
        assert!(create_object("HTTP").is_ok());
        assert!(create_object("http").is_ok());
        assert!(create_object("  Http  ").is_ok());
        assert!(create_object("scripting.filesystemobject").is_ok());
    }

    #[test]
    fn every_advertised_name_and_alias_resolves() {
        for (canonical, aliases) in OBJECT_TYPES {
            assert!(
                create_object(canonical).is_ok(),
                "advertised but not constructible: {canonical}"
            );
            for alias in *aliases {
                assert!(
                    create_object(alias).is_ok(),
                    "alias not constructible: {alias}"
                );
                assert!(
                    create_object(&alias.to_lowercase()).is_ok(),
                    "alias is not case-insensitive: {alias}"
                );
                assert_eq!(
                    type_name_of(&create_object(alias).unwrap()),
                    type_name_of(&create_object(canonical).unwrap()),
                    "{alias} builds something other than {canonical}"
                );
            }
        }
    }

    #[test]
    fn aliases_are_distinct_and_do_not_shadow_a_canonical_name() {
        let mut seen: Vec<String> = Vec::new();
        for (canonical, aliases) in OBJECT_TYPES {
            for name in std::iter::once(canonical).chain(aliases.iter()) {
                let lowered = name.to_ascii_lowercase();
                assert!(!seen.contains(&lowered), "{name} is claimed twice");
                seen.push(lowered);
            }
        }
    }

    #[test]
    fn an_unknown_object_type_lists_what_is_available() {
        // Databases are deliberately out of scope, so this is a permanently
        // unknown type to test the error message against.
        let error = create_object("ADODB.Connection").unwrap_err();
        assert_eq!(error.number, 429);
        assert!(error.message.contains("HTTP"), "{}", error.message);
        assert!(
            error.message.contains("ADODB.Connection"),
            "{}",
            error.message
        );
    }

    #[test]
    fn send_before_open_is_an_error() {
        let value = create_object("HTTP").unwrap();
        let mut http = object(&value).borrow_mut();
        let err = http.call_method("send", vec![]).unwrap_err();
        assert_eq!(err.number, 5);
    }

    #[test]
    fn open_records_the_request_and_properties_start_empty() {
        let value = create_object("HTTP").unwrap();
        let mut http = object(&value).borrow_mut();
        http.call_method(
            "open",
            vec![Variant::string("GET"), Variant::string("https://example.invalid/")],
        )
        .unwrap();
        assert_eq!(
            http.get_property("responsetext").unwrap().display_string(),
            ""
        );
        assert_eq!(http.get_property("status").unwrap().to_int().unwrap(), 0);
    }

    #[test]
    fn unknown_members_report_438() {
        let value = create_object("HTTP").unwrap();
        let http = object(&value).borrow();
        assert_eq!(http.get_property("nope").unwrap_err().number, 438);
        assert_eq!(http.member_kind("responseText"), Member::Property);
        assert_eq!(http.member_kind("Send"), Member::Method);
    }

    #[test]
    fn err_object_exposes_message_and_description() {
        let mut err = ErrObject::new();
        err.set_from(&BrtError::new(11, "Division by zero"), Some("lib/math.btm"));
        assert_eq!(
            err.get_property("message").unwrap().display_string(),
            "Division by zero"
        );
        assert_eq!(
            err.get_property("description").unwrap().display_string(),
            "Division by zero"
        );
        assert_eq!(err.get_property("number").unwrap().to_int().unwrap(), 11);
        assert_eq!(
            err.get_property("file").unwrap().display_string(),
            "lib/math.btm"
        );
    }
}
