//! Pure, index-only query functions — each a trivial pass over the
//! precomputed per-method facts (`chains`, `own_field_accesses`,
//! `foreign_accesses`) rather than a tree walk, matching
//! `autoreview-archgraph`'s own `fan_out`/`fan_in`-style one-liners over a
//! prebuilt map. Independently unit-testable from hand-constructed
//! `SymbolIndex` values — no parsing involved.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::model::{MethodDecl, NamedSlot, SymbolIndex};

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

/// Whether Data Clumps are compared across the whole index or only within
/// methods sharing the same directory — see the plan's own scope decision
/// (whole-index is the shipped default: maximizes recall, since a clump
/// shared between e.g. a controller and a service in different packages is
/// often the more interesting refactor case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClumpScope {
    WholeIndex,
    SameDirectory,
}

/// One method that participates in a data clump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClumpMember {
    pub owner_type: String,
    pub method: String,
    pub file: PathBuf,
    pub line: u32,
}

/// A recurring group of parameters (the clump itself) plus every distinct
/// method it was found in.
#[derive(Debug, Clone, PartialEq)]
pub struct DataClumpFinding {
    pub signature: Vec<NamedSlot>,
    pub methods: Vec<ClumpMember>,
}

fn signature_key(window: &[NamedSlot]) -> String {
    window.iter().map(|s| format!("{}:{}", s.name, s.type_text)).collect::<Vec<_>>().join(",")
}

fn scope_key(method: &MethodDecl, scope: ClumpScope) -> String {
    match scope {
        ClumpScope::WholeIndex => String::new(),
        ClumpScope::SameDirectory => method.file.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
    }
}

/// Data Clumps: an identical ordered, contiguous subsequence (length
/// `min_len`) of parameter `(name, type)` pairs recurring across
/// `min_methods` distinct methods (deduped by `(owner_type, name, file)`,
/// so two overlapping windows within the same method's own parameter list
/// don't inflate its count). A method with a longer parameter list can
/// contribute more than one candidate window (e.g. a 5-param method has
/// three length-3 windows); each window is judged as its own signature.
pub fn find_data_clumps(index: &SymbolIndex, min_len: usize, min_methods: usize, scope: ClumpScope) -> Vec<DataClumpFinding> {
    let mut scoped_groups: BTreeMap<String, Vec<&MethodDecl>> = BTreeMap::new();
    for method in index.methods() {
        scoped_groups.entry(scope_key(method, scope)).or_default().push(method);
    }

    let mut findings = Vec::new();
    for methods in scoped_groups.into_values() {
        let mut by_signature: BTreeMap<String, Vec<(&MethodDecl, &[NamedSlot])>> = BTreeMap::new();
        for method in &methods {
            if method.params.len() < min_len {
                continue;
            }
            for window in method.params.windows(min_len) {
                by_signature.entry(signature_key(window)).or_default().push((method, window));
            }
        }

        for occurrences in by_signature.into_values() {
            let mut seen: HashSet<(String, String, PathBuf)> = HashSet::new();
            let mut members = Vec::new();
            let mut signature: Option<Vec<NamedSlot>> = None;
            for (method, window) in occurrences {
                let key = (method.owner_type.clone(), method.name.clone(), method.file.clone());
                if seen.insert(key) {
                    members.push(ClumpMember { owner_type: method.owner_type.clone(), method: method.name.clone(), file: method.file.clone(), line: method.start_line });
                    signature.get_or_insert_with(|| window.to_vec());
                }
            }
            if members.len() >= min_methods {
                findings.push(DataClumpFinding { signature: signature.unwrap_or_default(), methods: members });
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

    fn method_with_params(owner_type: &str, file: &str, name: &str, params: &[(&str, &str)]) -> MethodDecl {
        MethodDecl {
            name: name.to_string(),
            owner_type: owner_type.to_string(),
            file: PathBuf::from(file),
            start_line: 1,
            end_line: 2,
            params: params.iter().map(|(n, t)| NamedSlot { name: n.to_string(), type_text: t.to_string() }).collect(),
            return_type_text: None,
            own_field_accesses: vec![],
            foreign_accesses: vec![],
            chains: vec![],
        }
    }

    fn index_with_methods(methods: Vec<MethodDecl>) -> SymbolIndex {
        let mut types: Vec<TypeDecl> = Vec::new();
        for m in methods {
            if let Some(t) = types.iter_mut().find(|t| t.name == m.owner_type && t.file == m.file) {
                t.methods.push(m);
            } else {
                types.push(TypeDecl { name: m.owner_type.clone(), file: m.file.clone(), start_line: 1, fields: vec![], methods: vec![m] });
            }
        }
        SymbolIndex { types }
    }

    const CLUMP: &[(&str, &str)] = &[("name", "String"), ("id", "int"), ("active", "bool")];

    #[test]
    fn a_clump_recurring_across_min_methods_is_reported() {
        let index = index_with_methods(vec![
            method_with_params("A", "a.java", "one", CLUMP),
            method_with_params("B", "b.java", "two", CLUMP),
            method_with_params("C", "c.java", "three", CLUMP),
        ]);
        let findings = find_data_clumps(&index, 3, 3, ClumpScope::WholeIndex);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
        assert_eq!(findings[0].methods.len(), 3);
    }

    #[test]
    fn a_clump_below_min_methods_is_not_reported() {
        let index = index_with_methods(vec![method_with_params("A", "a.java", "one", CLUMP), method_with_params("B", "b.java", "two", CLUMP)]);
        assert!(find_data_clumps(&index, 3, 3, ClumpScope::WholeIndex).is_empty());
    }

    #[test]
    fn whole_index_scope_finds_a_clump_split_across_directories() {
        let index = index_with_methods(vec![
            method_with_params("A", "pkg1/a.java", "one", CLUMP),
            method_with_params("B", "pkg2/b.java", "two", CLUMP),
            method_with_params("C", "pkg3/c.java", "three", CLUMP),
        ]);
        assert_eq!(find_data_clumps(&index, 3, 3, ClumpScope::WholeIndex).len(), 1);
    }

    #[test]
    fn same_directory_scope_does_not_find_a_clump_split_across_directories() {
        let index = index_with_methods(vec![
            method_with_params("A", "pkg1/a.java", "one", CLUMP),
            method_with_params("B", "pkg2/b.java", "two", CLUMP),
            method_with_params("C", "pkg3/c.java", "three", CLUMP),
        ]);
        assert!(find_data_clumps(&index, 3, 3, ClumpScope::SameDirectory).is_empty());
    }

    #[test]
    fn same_directory_scope_finds_a_clump_confined_to_one_directory() {
        let index = index_with_methods(vec![
            method_with_params("A", "pkg1/a.java", "one", CLUMP),
            method_with_params("B", "pkg1/b.java", "two", CLUMP),
            method_with_params("C", "pkg1/c.java", "three", CLUMP),
        ]);
        assert_eq!(find_data_clumps(&index, 3, 3, ClumpScope::SameDirectory).len(), 1);
    }

    #[test]
    fn a_method_shorter_than_min_len_never_contributes_a_window() {
        let index = index_with_methods(vec![
            method_with_params("A", "a.java", "one", &[("x", "int"), ("y", "int")]),
            method_with_params("B", "b.java", "two", &[("x", "int"), ("y", "int")]),
            method_with_params("C", "c.java", "three", &[("x", "int"), ("y", "int")]),
        ]);
        assert!(find_data_clumps(&index, 3, 3, ClumpScope::WholeIndex).is_empty());
    }
}
