use super::{check_with_budget, CallOrderHit, CallOrderSpec, KeyedUnit, PATH_BUDGET};

/// Whether keyed control-flow lowering exhausted its finally-copy budget.
pub fn keyed_flow_exhausted<S>(graph: &crate::cfg::Cfg<S>) -> bool {
    graph.keyed_flow == Some(0)
}

/// Follow direct helper calls across the supplied source units.
pub fn check_keyed_units(
    spec: &CallOrderSpec,
    units: &[KeyedUnit<'_, '_, '_>],
) -> Vec<CallOrderHit> {
    check_keyed_units_with_coverage(spec, units).hits
}

/// Findings and scopes that were declined or exhausted during analysis.
pub struct KeyedCheckResult {
    pub hits: Vec<CallOrderHit>,
    pub limitations: Vec<KeyedLimitation>,
}

pub struct KeyedLimitation {
    pub reason: &'static str,
    pub path: Option<String>,
    pub line: Option<u32>,
}

pub fn check_keyed_units_with_coverage(
    spec: &CallOrderSpec,
    units: &[KeyedUnit<'_, '_, '_>],
) -> KeyedCheckResult {
    check_with_budget(spec, units, PATH_BUDGET)
}

