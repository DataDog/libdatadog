use std::collections::HashMap;
use std::sync::Arc;

use crate::rules_based::Str;

use crate::rules_based::Attribute;

/// `Subject` is a bundle of subject attributes and a key.
#[derive(Debug)]
pub struct EvaluationContext {
    /// Subject key encoded as attribute value. Known to be `AttributeValue::String`. This is
    /// done to allow returning subject key as an attribute when rule references "id".
    targeting_key: Attribute,
    attributes: Arc<HashMap<Str, Attribute>>,
}

impl EvaluationContext {
    pub fn new(key: Str, attributes: Arc<HashMap<Str, Attribute>>) -> EvaluationContext {
        EvaluationContext {
            targeting_key: Attribute::from(key),
            attributes,
        }
    }

    pub fn targeting_key(&self) -> &Str {
        let Some(s) = self.targeting_key.as_str() else {
            unreachable!("Subject::key is always encoded as string attribute");
        };
        s
    }

    /// Get subject attribute.
    ///
    /// If attribute `name` is `"id"` and there's no explicit attribute with this name, return
    /// subject key instead.
    pub fn get_attribute(&self, name: &str) -> Option<&Attribute> {
        let value = self.attributes.get(name);
        if value.is_some() {
            return value;
        }

        if name == "id" {
            return Some(&self.targeting_key);
        }

        None
    }
}
