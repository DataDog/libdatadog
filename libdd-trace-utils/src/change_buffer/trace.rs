use std::collections::HashMap;

#[derive(Default)]
pub struct Trace<T> {
    pub meta: HashMap<T, T>,
    pub metrics: HashMap<T, f64>,
    pub origin: Option<T>,
    pub sampling_rule_decision: Option<f64>,
    pub sampling_limit_decision: Option<f64>,
    pub sampling_agent_decision: Option<f64>,
}

impl<T: Default> Trace<T> {
    pub fn new() -> Self {
        Default::default()
    }
}
