use std::sync::Arc;

use crate::rules_based::Str;

use crate::rules_based::{AttributeValue, Attributes};

/// `Subject` is a bundle of subject attributes and a key.
#[derive(Debug)]
pub(super) struct Subject {
    /// Subject key encoded as attribute value. Known to be `AttributeValue::String`. This is
    /// done to allow returning subject key as an attribute when rule references "id".
    key: AttributeValue,
    attributes: Arc<Attributes>,
}

impl Subject {
    pub fn new(key: Str, attributes: Arc<Attributes>) -> Subject {
        Subject {
            key: AttributeValue::from(key),
            attributes,
        }
    }

    pub fn key(&self) -> &Str {
        let Some(s) = self.key.as_str() else {
            unreachable!("Subject::key is always encoded as categorical string attribute");
        };
        s
    }

    /// Get subject attribute.
    ///
    /// If attribute `name` is `"id"` and there's no explicit attribute with this name, return
    /// subject key instead. This is a standard Eppo behavior when evaluation rules.
    pub fn get_attribute(&self, name: &str) -> Option<&AttributeValue> {
        let value = self.attributes.get(name);
        if value.is_some() {
            return value;
        }

        if name == "id" {
            return Some(&self.key);
        }

        None
    }
}
