//! The `Variant` value type and the VB6 coercion quirks.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;

use crate::error::BrtError;
use crate::objects::BartObject;

/// Every Bartelang value is an implicitly-typed `Variant`, exactly as in
/// Visual Basic 6 with `Option Explicit` off.
pub enum Variant {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    /// The state of a freshly `Dim`-ed variable, and of `Nothing`.
    Empty,
    /// A platform object, such as the HTTP client or a dictionary.
    Object(Rc<RefCell<dyn BartObject>>),
    /// A 1-based array.  Cloning shares the same storage, exactly as handing a
    /// VB6 array to a procedure did.
    Array(Rc<RefCell<Vec<Variant>>>),
    /// An instance of a user-defined `Type`.  Unlike arrays and objects, this
    /// copies on clone - records are value types, as they were in VB6.
    Record(Rc<RefCell<crate::record::Record>>),
    /// VB6's `Null`: a value that is present but carries nothing.  Distinct from
    /// `Empty`, which is what an untouched `Dim` holds.  Nothing in Bartelang
    /// produces a `Null` on its own - only the `Null` literal does - and using it
    /// in arithmetic or a comparison raises error 94 rather than propagating.
    Null,
}

/// `Clone` is written out by hand because records copy while everything else
/// shares: cloning a record deep-copies its fields, so handing one to a
/// procedure cannot modify the caller's.
impl Clone for Variant {
    fn clone(&self) -> Self {
        match self {
            Variant::Int(value) => Variant::Int(*value),
            Variant::Float(value) => Variant::Float(*value),
            Variant::String(value) => Variant::String(value.clone()),
            Variant::Bool(value) => Variant::Bool(*value),
            Variant::Empty => Variant::Empty,
            Variant::Object(handle) => Variant::Object(Rc::clone(handle)),
            Variant::Array(storage) => Variant::Array(Rc::clone(storage)),
            Variant::Record(record) => {
                Variant::Record(Rc::new(RefCell::new(record.borrow().clone())))
            }
            Variant::Null => Variant::Null,
        }
    }
}

impl fmt::Debug for Variant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Variant::Int(v) => write!(f, "Int({v})"),
            Variant::Float(v) => write!(f, "Float({v})"),
            Variant::String(v) => write!(f, "String({v:?})"),
            Variant::Bool(v) => write!(f, "Bool({v})"),
            Variant::Empty => write!(f, "Empty"),
            Variant::Object(o) => write!(f, "Object({})", o.borrow().type_name()),
            Variant::Array(a) => write!(f, "Array(len={})", a.borrow().len()),
            Variant::Record(r) => write!(f, "Record({})", r.borrow().type_name),
            Variant::Null => write!(f, "Null"),
        }
    }
}

/// A number in either of Bartelang's two numeric flavours.
#[derive(Clone, Copy, Debug)]
pub enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    /// This number as an `f64`, whichever flavour it is.
    pub fn as_f64(self) -> f64 {
        match self {
            Num::Int(v) => v as f64,
            Num::Float(v) => v,
        }
    }

    /// This number as an `i64`, truncating a `Float`.
    pub fn as_i64(self) -> i64 {
        match self {
            Num::Int(v) => v,
            Num::Float(v) => v as i64,
        }
    }

    /// Whether this came from an integer value rather than a double.
    pub fn is_int(self) -> bool {
        matches!(self, Num::Int(_))
    }
}

impl Variant {
    /// A `String` variant, for building values in Rust without the ceremony.
    pub fn string(value: impl Into<String>) -> Self {
        Variant::String(value.into())
    }

    /// The VB type name, used in error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Variant::Int(_) => "Integer",
            Variant::Float(_) => "Double",
            Variant::String(_) => "String",
            Variant::Bool(_) => "Boolean",
            Variant::Empty => "Empty",
            Variant::Object(_) => "Object",
            Variant::Array(_) => "Array",
            Variant::Record(_) => "Record",
            Variant::Null => "Null",
        }
    }

    /// VB truthiness: `0`, `Empty`, `False` and `""` are False; all else True.
    pub fn truthy(&self) -> bool {
        match self {
            Variant::Bool(b) => *b,
            Variant::Int(v) => *v != 0,
            Variant::Float(v) => *v != 0.0,
            Variant::String(s) => !s.is_empty(),
            Variant::Empty => false,
            Variant::Object(_) => true,
            // VB6 refuses to convert an array to a Boolean; truthy() cannot
            // fail, so an array simply counts as present.
            Variant::Array(_) => true,
            Variant::Record(_) => true,
            Variant::Null => false,
        }
    }

    /// Coerce to a number, applying VB's implicit conversions.
    ///
    /// `Empty` is 0, `True` is -1 (the VBA convention), numeric strings are
    /// parsed, and anything else is a type mismatch.
    pub fn to_num(&self) -> Result<Num, BrtError> {
        match self {
            Variant::Int(v) => Ok(Num::Int(*v)),
            Variant::Float(v) => Ok(Num::Float(*v)),
            Variant::Bool(b) => Ok(Num::Int(if *b { -1 } else { 0 })),
            Variant::Empty => Ok(Num::Int(0)),
            Variant::String(s) => {
                let trimmed = s.trim();
                if let Ok(v) = trimmed.parse::<i64>() {
                    Ok(Num::Int(v))
                } else if let Ok(v) = trimmed.parse::<f64>() {
                    Ok(Num::Float(v))
                } else {
                    Err(BrtError::type_mismatch(format!(
                        "Cannot convert string {s:?} to a number"
                    )))
                }
            }
            Variant::Object(o) => Err(BrtError::object_required(format!(
                "'{}' object cannot be used in a numeric expression",
                o.borrow().type_name()
            ))),
            Variant::Array(_) => Err(BrtError::type_mismatch(
                "An array cannot be used in a numeric expression",
            )),
            Variant::Null => Err(BrtError::null_use("a numeric expression")),
            Variant::Record(record) => Err(BrtError::type_mismatch(format!(
                "A {} record cannot be used in a numeric expression",
                record.borrow().type_name
            ))),
        }
    }

    /// Coerces to an integer, truncating a double and applying [`Variant::to_num`]'
    /// rules to everything else.
    pub fn to_int(&self) -> Result<i64, BrtError> {
        Ok(self.to_num()?.as_i64())
    }

    /// Coerces to a double, via [`Variant::to_num`].
    pub fn to_float(&self) -> Result<f64, BrtError> {
        Ok(self.to_num()?.as_f64())
    }

    /// Strict string coercion, used by the `&` operator.  Objects have no
    /// meaningful text form and raise 424, matching VB6.
    pub fn as_string(&self) -> Result<String, BrtError> {
        match self {
            Variant::Object(o) => Err(BrtError::object_required(format!(
                "'{}' object doesn't support string concatenation",
                o.borrow().type_name()
            ))),
            Variant::Array(_) => Err(BrtError::type_mismatch(
                "An array cannot be used in a string expression",
            )),
            // VB6's `&` treats Null as an empty string, and that one leniency is
            // worth keeping: it is how period code builds messages.
            Variant::Null => Ok(String::new()),
            Variant::Record(record) => Err(BrtError::type_mismatch(format!(
                "A {} record cannot be used in a string expression",
                record.borrow().type_name
            ))),
            other => Ok(other.display_string()),
        }
    }

    /// Lenient rendering, used for `Debug.Print` and interpolation.
    pub fn display_string(&self) -> String {
        match self {
            Variant::Int(v) => v.to_string(),
            Variant::Float(v) => format_float(*v),
            Variant::String(s) => s.clone(),
            Variant::Bool(b) => {
                if *b {
                    "True".to_string()
                } else {
                    "False".to_string()
                }
            }
            Variant::Empty => String::new(),
            Variant::Object(o) => format!("<{} object>", o.borrow().type_name()),
            Variant::Array(a) => format!("Array(1 To {})", a.borrow().len()),
            Variant::Null => "Null".to_string(),
            Variant::Record(record) => format!("<{} record>", record.borrow().type_name),
        }
    }
}

/// VB renders whole doubles without a trailing `.0`.
pub fn format_float(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Order two variants the way VB6's default (binary) comparison does.
///
/// Two strings always compare as strings.  Otherwise Bartelang tries a numeric
/// comparison first and only falls back to text when a side refuses to be a
/// number.
pub fn compare(left: &Variant, right: &Variant) -> Result<Ordering, BrtError> {
    // VB6 compared Null to Null and got Null back, which made `If x = Null`
    // silently false.  Bartelang refuses the comparison and points at IsNull.
    if matches!(left, Variant::Null) || matches!(right, Variant::Null) {
        return Err(BrtError::null_use("a comparison"));
    }
    // VB6 refused to compare records outright; the alternative here would be
    // comparing their printed forms and calling two different points equal.
    if matches!(left, Variant::Record(_)) || matches!(right, Variant::Record(_)) {
        return Err(BrtError::type_mismatch("Records cannot be compared"));
    }
    if let (Variant::String(a), Variant::String(b)) = (left, right) {
        return Ok(a.as_bytes().cmp(b.as_bytes()));
    }
    match (left.to_num(), right.to_num()) {
        (Ok(a), Ok(b)) => Ok(match (a, b) {
            (Num::Int(x), Num::Int(y)) => x.cmp(&y),
            (x, y) => x
                .as_f64()
                .partial_cmp(&y.as_f64())
                .unwrap_or(Ordering::Equal),
        }),
        _ => Ok(left.display_string().cmp(&right.display_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness_follows_the_spec() {
        assert!(!Variant::Int(0).truthy());
        assert!(!Variant::Empty.truthy());
        assert!(!Variant::string("").truthy());
        assert!(!Variant::Bool(false).truthy());
        assert!(Variant::string("0").truthy());
        assert!(Variant::Int(-3).truthy());
    }

    #[test]
    fn strings_coerce_to_numbers_when_possible() {
        assert_eq!(Variant::string(" 42 ").to_int().unwrap(), 42);
        assert!(matches!(
            Variant::string("3.5").to_num().unwrap(),
            Num::Float(v) if v == 3.5
        ));
        assert_eq!(Variant::string("abc").to_num().unwrap_err().number, 13);
    }

    #[test]
    fn name_confusion() {
        assert_eq!(Variant::Bool(true).to_int().unwrap(), -1);
        assert_eq!(Variant::Empty.to_int().unwrap(), 0);
    }

    #[test]
    fn whole_floats_print_without_a_decimal_point() {
        assert_eq!(Variant::Float(5.0).display_string(), "5");
        assert_eq!(Variant::Float(5.25).display_string(), "5.25");
    }

    #[test]
    fn comparison_prefers_string_semantics_for_two_strings() {
        assert_eq!(
            compare(&Variant::string("10"), &Variant::string("9")).unwrap(),
            Ordering::Less
        );
        assert_eq!(
            compare(&Variant::Int(10), &Variant::Int(9)).unwrap(),
            Ordering::Greater
        );
    }
}
