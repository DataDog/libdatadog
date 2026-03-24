use rustc_hash::FxHashMap;

#[derive(Default)]
pub struct Trace<T> {
    pub meta: FxHashMap<T, T>,
    pub metrics: FxHashMap<T, f64>,
    pub origin: Option<T>,
    pub sampling_rule_decision: Option<f64>,
    pub sampling_limit_decision: Option<f64>,
    pub sampling_agent_decision: Option<f64>,
    pub span_count: usize,
}
