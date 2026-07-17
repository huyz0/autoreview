//! Pure, index-only query functions — each a trivial pass over the
//! precomputed per-method facts (`chains`, `own_field_accesses`,
//! `foreign_accesses`) rather than a tree walk, matching
//! `autoreview-archgraph`'s own `fan_out`/`fan_in`-style one-liners over a
//! prebuilt map. Independently unit-testable from hand-constructed
//! `SymbolIndex` values — no parsing involved.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::model::SymbolIndex;

/// One message-chain finding: a call chain at or above the reporting
/// threshold, anchored at the method it was found in.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainFinding {
    pub file: PathBuf,
    pub line: u32,
    pub owner_type: String,
    pub method: String,
    pub depth: usize,
    pub chain_text: String,
}

/// Every call chain across the whole index at or above `min_depth` links
/// (Fowler's Message Chains: `a.b().c().d()`). Extraction records every
/// chain regardless of depth; filtering by threshold happens here, at
/// query time, so the threshold is tunable without re-extracting.
pub fn find_message_chains(index: &SymbolIndex, min_depth: usize) -> Vec<ChainFinding> {
    index
        .methods()
        .flat_map(|m| {
            m.chains.iter().filter(move |c| c.depth >= min_depth).map(move |c| ChainFinding {
                file: m.file.clone(),
                line: c.line,
                owner_type: m.owner_type.clone(),
                method: m.name.clone(),
                depth: c.depth,
                chain_text: if c.root_text.is_empty() { c.member_names.join(".") } else { format!("{}.{}", c.root_text, c.member_names.join(".")) },
            })
        })
        .collect()
}

/// One Feature Envy finding: a method whose accesses to a single foreign
/// type's members outnumber its own-field accesses by at least `margin`,
/// clearing an absolute floor (`min_foreign_accesses`) so a trivial
/// delegator method doesn't trip it.
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureEnvyFinding {
    pub file: PathBuf,
    pub line: u32,
    pub owner_type: String,
    pub method: String,
    pub envied_type: String,
    pub envied_access_count: usize,
    pub own_access_count: usize,
}

/// Feature Envy: a method envies a foreign type when accesses to it (via a
/// method parameter — see the plan's scope decision, locals aren't
/// tracked) reach `min_foreign_accesses` and exceed the method's own-field
/// access count by at least `margin`. Foreign accesses are grouped by the
/// parameter's declared type text (not by parameter name — two
/// differently-named params of the same type both count toward that
/// type's envy), so a method juggling several unrelated foreign types is
/// judged per type, not by a single combined count.
pub fn find_feature_envy(index: &SymbolIndex, min_foreign_accesses: usize, margin: i64) -> Vec<FeatureEnvyFinding> {
    let mut findings = Vec::new();
    for m in index.methods() {
        if m.foreign_accesses.is_empty() {
            continue;
        }
        let own_count = m.own_field_accesses.len();

        let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
        for access in &m.foreign_accesses {
            let Some(receiver_type) = &access.receiver_type else { continue };
            *by_type.entry(receiver_type.clone()).or_insert(0) += 1;
        }

        for (envied_type, envied_access_count) in by_type {
            if envied_access_count >= min_foreign_accesses && envied_access_count as i64 - own_count as i64 >= margin {
                findings.push(FeatureEnvyFinding {
                    file: m.file.clone(),
                    line: m.start_line,
                    owner_type: m.owner_type.clone(),
                    method: m.name.clone(),
                    envied_type,
                    envied_access_count,
                    own_access_count: own_count,
                });
            }
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AccessRef, CallChain, ForeignAccessRef, MethodDecl, TypeDecl};
    use std::path::PathBuf;

    fn method_with_chain(depth: usize) -> MethodDecl {
        MethodDecl {
            name: "chain".to_string(),
            owner_type: "Widget".to_string(),
            file: PathBuf::from("Widget.java"),
            start_line: 1,
            end_line: 2,
            params: vec![],
            return_type_text: None,
            own_field_accesses: vec![],
            foreign_accesses: vec![],
            chains: vec![CallChain {
                root_text: "owner".to_string(),
                depth,
                line: 5,
                member_names: (0..depth).map(|i| format!("m{i}")).collect(),
            }],
        }
    }

    fn index_with_method(method: MethodDecl) -> SymbolIndex {
        SymbolIndex { types: vec![TypeDecl { name: "Widget".to_string(), file: PathBuf::from("Widget.java"), start_line: 1, fields: vec![], methods: vec![method] }] }
    }

    #[test]
    fn a_chain_at_or_above_the_threshold_is_reported() {
        let index = index_with_method(method_with_chain(3));
        let findings = find_message_chains(&index, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].depth, 3);
        assert_eq!(findings[0].owner_type, "Widget");
        assert_eq!(findings[0].chain_text, "owner.m0.m1.m2");
    }

    #[test]
    fn a_chain_below_the_threshold_is_not_reported() {
        let index = index_with_method(method_with_chain(2));
        assert!(find_message_chains(&index, 3).is_empty());
    }

    #[test]
    fn a_chain_exactly_at_the_threshold_is_reported() {
        let index = index_with_method(method_with_chain(3));
        assert_eq!(find_message_chains(&index, 3).len(), 1);
    }

    #[test]
    fn an_index_with_no_methods_reports_nothing() {
        let index = SymbolIndex::default();
        assert!(find_message_chains(&index, 1).is_empty());
    }

    fn method_with_accesses(own_count: usize, foreign_type_counts: &[(&str, usize)]) -> MethodDecl {
        let own_field_accesses = (0..own_count).map(|i| AccessRef { field_name: format!("f{i}"), line: 1 }).collect();
        let mut foreign_accesses = Vec::new();
        for (type_name, count) in foreign_type_counts {
            for i in 0..*count {
                foreign_accesses.push(ForeignAccessRef { receiver_name: format!("p{i}"), receiver_type: Some(type_name.to_string()), member_name: format!("m{i}"), line: 1 });
            }
        }
        MethodDecl {
            name: "envy".to_string(),
            owner_type: "Widget".to_string(),
            file: PathBuf::from("Widget.java"),
            start_line: 1,
            end_line: 2,
            params: vec![],
            return_type_text: None,
            own_field_accesses,
            foreign_accesses,
            chains: vec![],
        }
    }

    #[test]
    fn a_method_clearing_both_the_floor_and_the_margin_is_reported() {
        // min_foreign_accesses=3, margin=2: 3 foreign vs 0 own clears both.
        let index = index_with_method(method_with_accesses(0, &[("Customer", 3)]));
        let findings = find_feature_envy(&index, 3, 2);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].envied_type, "Customer");
        assert_eq!(findings[0].envied_access_count, 3);
        assert_eq!(findings[0].own_access_count, 0);
    }

    #[test]
    fn a_method_below_the_absolute_floor_is_not_reported_even_with_zero_own_accesses() {
        let index = index_with_method(method_with_accesses(0, &[("Customer", 2)]));
        assert!(find_feature_envy(&index, 3, 2).is_empty());
    }

    #[test]
    fn a_method_at_the_floor_but_under_the_margin_is_not_reported() {
        // 3 foreign vs 2 own: clears the floor (3) but margin (3-2=1) < 2.
        let index = index_with_method(method_with_accesses(2, &[("Customer", 3)]));
        assert!(find_feature_envy(&index, 3, 2).is_empty());
    }

    #[test]
    fn a_method_exactly_at_the_margin_boundary_is_reported() {
        // 4 foreign vs 2 own: margin exactly 2.
        let index = index_with_method(method_with_accesses(2, &[("Customer", 4)]));
        assert_eq!(find_feature_envy(&index, 3, 2).len(), 1);
    }

    #[test]
    fn foreign_accesses_to_different_types_are_judged_independently() {
        let index = index_with_method(method_with_accesses(0, &[("Customer", 3), ("Invoice", 1)]));
        let findings = find_feature_envy(&index, 3, 2);
        assert_eq!(findings.len(), 1, "Invoice's count (1) shouldn't clear the floor on its own");
        assert_eq!(findings[0].envied_type, "Customer");
    }

    #[test]
    fn a_method_with_no_foreign_accesses_reports_nothing() {
        let index = index_with_method(method_with_accesses(5, &[]));
        assert!(find_feature_envy(&index, 3, 2).is_empty());
    }
}
