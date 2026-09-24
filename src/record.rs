//! User-defined record types: `Type ... End Type`.
//!
//! Records are *value* types, as they were in VB6: assigning one copies its
//! fields, so a procedure can be handed a record and modify it without touching
//! the caller's.  That is the opposite of arrays, which Bartelang shares by
//! reference, and it is why `Variant` has a hand-written `Clone`.

use std::collections::HashMap;

use crate::error::BrtError;
use crate::value::Variant;

/// One live instance of a `Type`.
#[derive(Clone)]
pub struct Record {
    pub type_name: String,
    pub fields: HashMap<String, Variant>,
}

impl Record {
    /// An empty record of the named type.
    pub fn new(type_name: String) -> Self {
        Record {
            type_name,
            fields: HashMap::new(),
        }
    }

    /// Reads a field, or reports error 438 when there is no such field.
    pub fn get(&self, field: &str) -> Result<Variant, BrtError> {
        self.fields
            .get(field)
            .cloned()
            .ok_or_else(|| BrtError::no_such_member(&self.type_name, field))
    }

    /// Writes a field, refusing names the type does not declare.
    pub fn set(&mut self, field: &str, value: Variant) -> Result<(), BrtError> {
        match self.fields.get_mut(field) {
            Some(slot) => {
                *slot = value;
                Ok(())
            }
            None => Err(BrtError::no_such_member(&self.type_name, field)),
        }
    }
}

/// A `Type` declaration, with any field array sizes already evaluated.
pub(crate) struct TypeDef {
    pub name: String,
    /// Field name, and the fixed length of an array field when it has one.
    pub fields: Vec<(String, Option<usize>)>,
}

impl TypeDef {
    /// A fresh instance with every field at its initial value.
    pub fn instantiate(&self) -> Variant {
        let mut record = Record::new(self.name.clone());
        for (field, array_size) in &self.fields {
            let value = match array_size {
                Some(size) => Variant::Array(std::rc::Rc::new(std::cell::RefCell::new(vec![
                    Variant::Empty;
                    *size
                ]))),
                None => Variant::Empty,
            };
            record.fields.insert(field.clone(), value);
        }
        Variant::Record(std::rc::Rc::new(std::cell::RefCell::new(record)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point() -> TypeDef {
        TypeDef {
            name: "Point".to_string(),
            fields: vec![("x".to_string(), None), ("y".to_string(), None)],
        }
    }

    #[test]
    fn an_instance_has_every_field() {
        let Variant::Record(record) = point().instantiate() else {
            panic!("expected a record");
        };
        let record = record.borrow();
        assert_eq!(record.type_name, "Point");
        assert!(matches!(record.get("x").unwrap(), Variant::Empty));
        assert!(matches!(record.get("y").unwrap(), Variant::Empty));
    }

    #[test]
    fn unknown_fields_are_reported() {
        let Variant::Record(record) = point().instantiate() else {
            panic!("expected a record");
        };
        let mut record = record.borrow_mut();
        assert_eq!(record.get("z").unwrap_err().number, 438);
        assert_eq!(record.set("z", Variant::Int(1)).unwrap_err().number, 438);
    }

    #[test]
    fn array_fields_get_their_own_storage() {
        let definition = TypeDef {
            name: "Bag".to_string(),
            fields: vec![("items".to_string(), Some(3))],
        };
        let Variant::Record(first) = definition.instantiate() else {
            panic!("expected a record");
        };
        let Variant::Record(second) = definition.instantiate() else {
            panic!("expected a record");
        };
        let Variant::Array(first_array) = first.borrow().get("items").unwrap() else {
            panic!("expected an array field");
        };
        let Variant::Array(second_array) = second.borrow().get("items").unwrap() else {
            panic!("expected an array field");
        };
        first_array.borrow_mut()[0] = Variant::Int(7);
        assert!(matches!(second_array.borrow()[0], Variant::Empty));
    }

    #[test]
    fn cloning_a_record_copies_its_fields() {
        let value = point().instantiate();
        let Variant::Record(original) = &value else {
            panic!("expected a record");
        };
        original.borrow_mut().set("x", Variant::Int(1)).unwrap();

        // Cloning the `Variant` must copy the record; cloning the `Rc` would
        // share it, which is exactly what this test is guarding against.
        let copy = value.clone();
        let Variant::Record(copy) = copy else {
            panic!("expected a record");
        };
        copy.borrow_mut().set("x", Variant::Int(2)).unwrap();

        assert_eq!(original.borrow().get("x").unwrap().to_int().unwrap(), 1);
        assert_eq!(copy.borrow().get("x").unwrap().to_int().unwrap(), 2);
    }
}
