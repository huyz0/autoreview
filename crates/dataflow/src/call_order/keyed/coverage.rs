use super::{check_with_budget, CallOrderHit, CallOrderSpec, KeyedUnit, PATH_BUDGET};
use std::collections::{BTreeMap, BTreeSet};

type SourcePositions = BTreeMap<usize, BTreeSet<usize>>;
type ComponentMap = BTreeMap<usize, BTreeMap<usize, super::super::ResultComponents>>;

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
    check_keyed_units_with_verified_components(spec, units, verified, &BTreeMap::new())
}


/// Analyze independently verified sources with per-result component identities.
pub fn check_keyed_units_with_verified_components(
    spec: &CallOrderSpec, units: &[KeyedUnit<'_, '_, '_>],
    verified: super::super::result_use::FactorySourceDiscovery,
    components: &ComponentMap,
) -> KeyedCheckResult {
    let mut limitations = verified.limitations;
    if !spec.tracks_result_use() {
        limitations.push(KeyedLimitation { reason: "unsupported_verified_sources", path: None, line: None });
        return KeyedCheckResult { hits: Vec::new(), limitations };
    }
    let (candidates, valid) = sanitize_components(units, verified.candidates, components, &mut limitations);
    let (hits, exhausted, found) = super::super::result_use::check_units_with_spans(spec, units, &candidates, &valid, &verified.spans);
    limitations.extend(found);
    if exhausted { limitations.push(KeyedLimitation { reason: "helper_summary_limit", path: None, line: None }); }
    KeyedCheckResult { hits, limitations }
}

fn sanitize_components(
    units: &[KeyedUnit<'_, '_, '_>], mut candidates: SourcePositions,
    components: &ComponentMap,
    limitations: &mut Vec<KeyedLimitation>,
) -> (SourcePositions, ComponentMap) {
    let mut valid = components.clone();
    for (&unit, shapes) in components {
        for (&start, shape) in shapes {
            let sound = units.get(unit).is_some_and(|item| start < item.source.len())
                && candidates.get(&unit).is_some_and(|sites| sites.contains(&start)) && valid_shape(shape);
            if sound { continue; }
            if let Some(sites) = candidates.get_mut(&unit) { sites.remove(&start); }
            if let Some(shapes) = valid.get_mut(&unit) { shapes.remove(&start); }
            limitations.push(KeyedLimitation { reason: "invalid_result_components", path: units.get(unit).map(|item| item.path.to_owned()), line: None });
        }
    }
    (candidates, valid)
}

fn valid_shape(shape: &super::super::ResultComponents) -> bool {
    shape.count > 1 && !shape.indices.is_empty()
        && shape.indices.iter().all(|index| *index < shape.count)
        && shape.indices.windows(2).all(|pair| pair[0] < pair[1])
}
