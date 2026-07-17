//! Pure, index-only query functions — each a trivial pass over the
//! precomputed per-method facts (`chains`, `own_field_accesses`,
//! `foreign_accesses`) rather than a tree walk, matching
//! `autoreview-archgraph`'s own `fan_out`/`fan_in`-style one-liners over a
//! prebuilt map. Independently unit-testable from hand-constructed
//! `SymbolIndex` values — no parsing involved.

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CallChain, MethodDecl, TypeDecl};
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
}
