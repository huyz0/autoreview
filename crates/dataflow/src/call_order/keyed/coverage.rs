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

/// Analyze call positions that the caller has independently verified as
/// result sources, retaining any discovery and verification limitations.
pub fn check_keyed_units_with_verified_sources(
    spec: &CallOrderSpec,
    units: &[KeyedUnit<'_, '_, '_>],
    verified: super::super::result_use::FactorySourceDiscovery,
) -> KeyedCheckResult {
    let mut limitations = verified.limitations;
    if !spec.tracks_result_use() {
        limitations.push(KeyedLimitation { reason: "unsupported_verified_sources", path: None, line: None });
        return KeyedCheckResult { hits: Vec::new(), limitations };
    }
    let (hits, exhausted, found) = super::super::result_use::check_units_with_sources(spec, units, &verified.candidates);
    limitations.extend(found);
    if exhausted { limitations.push(KeyedLimitation { reason: "helper_summary_limit", path: None, line: None }); }
    KeyedCheckResult { hits, limitations }
}

