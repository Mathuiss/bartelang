//! `Scripting.Dictionary` and `Collection`: the associative containers the
//! language was missing, which ruling out the counting, grouping and
//! de-duplication that shell-adjacent scripting runs on.

use std::collections::HashMap;

use crate::error::BrtError;
use crate::objects::{BartObject, Member};
use crate::value::Variant;

/// Keys are compared as text, so `Add 1, x` and `Exists "1"` agree.
fn key_of(value: &Variant) -> String {
    value.display_string()
}

/// An index-or-key argument: a number means a 1-based position.
fn position_or_key(value: &Variant) -> Result<usize, String> {
    match value {
        Variant::Int(index) => Ok(*index as usize),
        Variant::String(key) => Err(key.clone()),
        other => Err(key_of(other)),
    }
}

/// `CreateObject("Scripting.Dictionary")`
///
/// Insertion order is preserved, which is what makes `Keys`, `Items` and
/// `For Each` predictable.
#[derive(Default)]
pub struct Dictionary {
    entries: Vec<(String, Variant)>,
    index: HashMap<String, usize>,
}

impl Dictionary {
    /// An empty dictionary with no case folding, as `CreateObject` gives you.
    pub fn new() -> Self {
        Dictionary::default()
    }

    fn find(&self, key: &str) -> Option<usize> {
        self.index.get(key).copied()
    }

    fn insert(&mut self, key: String, value: Variant) {
        self.index.insert(key.clone(), self.entries.len());
        self.entries.push((key, value));
    }
}

impl BartObject for Dictionary {
    fn type_name(&self) -> String {
        "Scripting.Dictionary".to_string()
    }

    fn member_kind(&self, name: &str) -> Member {
        match name.to_ascii_lowercase().as_str() {
            "add" | "exists" | "item" | "remove" | "removeall" | "keys" | "items" => Member::Method,
            "count" => Member::Property,
            _ => Member::Unknown,
        }
    }

    fn call_method(&mut self, method: &str, args: Vec<Variant>) -> Result<Variant, BrtError> {
        match method.to_ascii_lowercase().as_str() {
            "add" => {
                if args.len() != 2 {
                    return Err(BrtError::wrong_arg_count("Dictionary.Add", args.len(), "2"));
                }
                let key = key_of(&args[0]);
                if self.find(&key).is_some() {
                    // VB6's error number for this exact situation.
                    return Err(BrtError::new(
                        457,
                        format!("This key is already associated with an element of this collection: {key}"),
                    ));
                }
                self.insert(key, args[1].clone());
                Ok(Variant::Empty)
            }
            "exists" => {
                if args.len() != 1 {
                    return Err(BrtError::wrong_arg_count("Dictionary.Exists", args.len(), "1"));
                }
                Ok(Variant::Bool(self.find(&key_of(&args[0])).is_some()))
            }
            "item" => {
                if args.len() != 1 {
                    return Err(BrtError::wrong_arg_count("Dictionary.Item", args.len(), "1"));
                }
                let key = key_of(&args[0]);
                match self.find(&key) {
                    Some(position) => Ok(self.entries[position].1.clone()),
                    // VB6 quietly created the key here.  Bartelang would rather
                    // surface the typo; use Exists to test for a key.
                    None => Err(BrtError::new(
                        457,
                        format!("Key not found in Dictionary: {key} (use Exists to test for it)"),
                    )),
                }
            }
            "remove" => {
                if args.len() != 1 {
                    return Err(BrtError::wrong_arg_count("Dictionary.Remove", args.len(), "1"));
                }
                let key = key_of(&args[0]);
                match self.find(&key) {
                    Some(position) => {
                        self.entries.remove(position);
                        // Positions after the hole have all shifted by one.
                        self.index.clear();
                        for (slot, (existing, _)) in self.entries.iter().enumerate() {
                            self.index.insert(existing.clone(), slot);
                        }
                        Ok(Variant::Empty)
                    }
                    None => Err(BrtError::new(
                        457,
                        format!("Key not found in Dictionary: {key}"),
                    )),
                }
            }
            "removeall" => {
                self.entries.clear();
                self.index.clear();
                Ok(Variant::Empty)
            }
            "keys" => Ok(Variant::Array(std::rc::Rc::new(std::cell::RefCell::new(
                self.entries
                    .iter()
                    .map(|(key, _)| Variant::String(key.clone()))
                    .collect(),
            )))),
            "items" => Ok(Variant::Array(std::rc::Rc::new(std::cell::RefCell::new(
                self.entries
                    .iter()
                    .map(|(_, value)| value.clone())
                    .collect(),
            )))),
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }

    fn get_property(&self, prop_name: &str) -> Result<Variant, BrtError> {
        match prop_name.to_ascii_lowercase().as_str() {
            "count" => Ok(Variant::Int(self.entries.len() as i64)),
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }

    /// `dict(key) = value` - VB6's default member, which creates the key.
    fn set_item(&mut self, key: Variant, value: Variant) -> Result<(), BrtError> {
        let key = key_of(&key);
        match self.find(&key) {
            Some(position) => {
                self.entries[position].1 = value;
                Ok(())
            }
            None => {
                self.insert(key, value);
                Ok(())
            }
        }
    }
}

/// `CreateObject("Collection")`
///
/// VB6's built-in collection.  It is 1-based and order-preserving, which is
/// exactly what the rest of the language promises.
#[derive(Default)]
pub struct Collection {
    elements: Vec<Variant>,
    keys: Vec<Option<String>>,
    index: HashMap<String, usize>,
}

impl Collection {
    /// An empty collection, as `CreateObject` gives you.
    pub fn new() -> Self {
        Collection::default()
    }

    /// Resolves a 1-based position or a string key to a 0-based slot.
    fn slot(&self, argument: &Variant) -> Result<usize, BrtError> {
        match position_or_key(argument) {
            Ok(position) => {
                if position < 1 || position > self.elements.len() {
                    return Err(BrtError::subscript(format!(
                        "Collection position {position} is outside 1..{}",
                        self.elements.len()
                    )));
                }
                Ok(position - 1)
            }
            Err(key) => self.index.get(&key).copied().ok_or_else(|| {
                BrtError::new(9, format!("Collection key not found: {key}"))
            }),
        }
    }
}

impl BartObject for Collection {
    fn type_name(&self) -> String {
        "Collection".to_string()
    }

    fn member_kind(&self, name: &str) -> Member {
        match name.to_ascii_lowercase().as_str() {
            "add" | "item" | "remove" => Member::Method,
            "count" => Member::Property,
            _ => Member::Unknown,
        }
    }

    fn call_method(&mut self, method: &str, args: Vec<Variant>) -> Result<Variant, BrtError> {
        match method.to_ascii_lowercase().as_str() {
            "add" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(BrtError::wrong_arg_count("Collection.Add", args.len(), "1 or 2"));
                }
                let key = match args.get(1) {
                    Some(value) => Some(value.as_string()?),
                    None => None,
                };
                if let Some(name) = &key {
                    if self.index.contains_key(name) {
                        return Err(BrtError::new(
                            457,
                            format!("This key is already associated with an element of this collection: {name}"),
                        ));
                    }
                    self.index.insert(name.clone(), self.elements.len());
                }
                self.elements.push(args[0].clone());
                self.keys.push(key);
                Ok(Variant::Empty)
            }
            "item" => {
                if args.len() != 1 {
                    return Err(BrtError::wrong_arg_count("Collection.Item", args.len(), "1"));
                }
                let slot = self.slot(&args[0])?;
                Ok(self.elements[slot].clone())
            }
            "remove" => {
                if args.len() != 1 {
                    return Err(BrtError::wrong_arg_count("Collection.Remove", args.len(), "1"));
                }
                let slot = self.slot(&args[0])?;
                self.elements.remove(slot);
                self.keys.remove(slot);
                // Rebuild the key index: everything after the hole moved up.
                self.index.clear();
                for (position, key) in self.keys.iter().enumerate() {
                    if let Some(name) = key {
                        self.index.insert(name.clone(), position);
                    }
                }
                Ok(Variant::Empty)
            }
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }

    fn get_property(&self, prop_name: &str) -> Result<Variant, BrtError> {
        match prop_name.to_ascii_lowercase().as_str() {
            "count" => Ok(Variant::Int(self.elements.len() as i64)),
            other => Err(BrtError::no_such_member(&self.type_name(), other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dictionary() -> Dictionary {
        Dictionary::new()
    }

    fn collection() -> Collection {
        Collection::new()
    }

    #[test]
    fn dictionary_adds_reads_and_counts_in_insertion_order() {
        let mut dict = dictionary();
        dict.call_method("add", vec![Variant::string("b"), Variant::Int(2)])
            .unwrap();
        dict.call_method("add", vec![Variant::string("a"), Variant::Int(1)])
            .unwrap();
        assert_eq!(dict.get_property("count").unwrap().to_int().unwrap(), 2);
        assert_eq!(
            dict.call_method("item", vec![Variant::string("a")])
                .unwrap()
                .to_int()
                .unwrap(),
            1
        );

        let keys = dict.call_method("keys", vec![]).unwrap();
        let Variant::Array(keys) = keys else {
            panic!("Keys should be an array");
        };
        let keys = keys.borrow();
        assert_eq!(keys[0].display_string(), "b");
        assert_eq!(keys[1].display_string(), "a");
    }

    #[test]
    fn dictionary_rejects_a_duplicate_key_and_a_missing_key() {
        let mut dict = dictionary();
        dict.call_method("add", vec![Variant::string("k"), Variant::Int(1)])
            .unwrap();
        assert_eq!(
            dict.call_method("add", vec![Variant::string("k"), Variant::Int(2)])
                .unwrap_err()
                .number,
            457
        );
        assert_eq!(
            dict.call_method("item", vec![Variant::string("nope")])
                .unwrap_err()
                .number,
            457
        );
        assert_eq!(
            dict.call_method("exists", vec![Variant::string("k")])
                .unwrap()
                .truthy(),
            true
        );
    }

    #[test]
    fn dictionary_assignment_creates_and_updates_keys() {
        let mut dict = dictionary();
        dict.set_item(Variant::string("new"), Variant::Int(1)).unwrap();
        dict.set_item(Variant::string("new"), Variant::Int(9)).unwrap();
        assert_eq!(dict.get_property("count").unwrap().to_int().unwrap(), 1);
        assert_eq!(
            dict.call_method("item", vec![Variant::string("new")])
                .unwrap()
                .to_int()
                .unwrap(),
            9
        );
    }

    #[test]
    fn dictionary_remove_keeps_the_remaining_positions_valid() {
        let mut dict = dictionary();
        for (key, value) in [("a", 1), ("b", 2), ("c", 3)] {
            dict.call_method("add", vec![Variant::string(key), Variant::Int(value)])
                .unwrap();
        }
        dict.call_method("remove", vec![Variant::string("a")]).unwrap();
        assert_eq!(dict.get_property("count").unwrap().to_int().unwrap(), 2);
        assert_eq!(
            dict.call_method("item", vec![Variant::string("c")])
                .unwrap()
                .to_int()
                .unwrap(),
            3
        );
        dict.call_method("removeall", vec![]).unwrap();
        assert_eq!(dict.get_property("count").unwrap().to_int().unwrap(), 0);
    }

    #[test]
    fn numeric_keys_are_text_so_they_agree_with_each_other() {
        let mut dict = dictionary();
        dict.call_method("add", vec![Variant::Int(1), Variant::string("one")])
            .unwrap();
        assert_eq!(
            dict.call_method("exists", vec![Variant::string("1")])
                .unwrap()
                .truthy(),
            true
        );
    }

    #[test]
    fn collection_is_one_based_and_order_preserving() {
        let mut collection = collection();
        for value in ["first", "second", "third"] {
            collection
                .call_method("add", vec![Variant::string(value)])
                .unwrap();
        }
        assert_eq!(collection.get_property("count").unwrap().to_int().unwrap(), 3);
        assert_eq!(
            collection
                .call_method("item", vec![Variant::Int(1)])
                .unwrap()
                .display_string(),
            "first"
        );
        assert_eq!(
            collection
                .call_method("item", vec![Variant::Int(3)])
                .unwrap()
                .display_string(),
            "third"
        );
        assert_eq!(
            collection
                .call_method("item", vec![Variant::Int(0)])
                .unwrap_err()
                .number,
            9
        );
    }

    #[test]
    fn collection_accepts_string_keys() {
        let mut collection = collection();
        collection
            .call_method("add", vec![Variant::string("v"), Variant::string("name")])
            .unwrap();
        assert_eq!(
            collection
                .call_method("item", vec![Variant::string("name")])
                .unwrap()
                .display_string(),
            "v"
        );
    }

    #[test]
    fn collection_remove_shifts_the_index() {
        let mut collection = collection();
        for value in ["a", "b", "c"] {
            collection
                .call_method("add", vec![Variant::string(value)])
                .unwrap();
        }
        collection
            .call_method("remove", vec![Variant::Int(1)])
            .unwrap();
        assert_eq!(
            collection
                .call_method("item", vec![Variant::Int(1)])
                .unwrap()
                .display_string(),
            "b"
        );
        assert_eq!(collection.get_property("count").unwrap().to_int().unwrap(), 2);
    }
}
