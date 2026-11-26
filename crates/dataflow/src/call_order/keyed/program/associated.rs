use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

use super::{exports, signature, CallSite, Export, KeyedUnit, Program, Scope};

#[derive(Default)]
pub(super) struct Associated {
    targets: BTreeMap<(usize, String, String, usize), Option<usize>>,
    local_types: BTreeSet<(usize, String)>,
}

impl Program<'_, '_, '_> {
    pub(super) fn resolve_named(&self, from: usize, name: &str, root: &str, site: &CallSite, arity: usize) -> Option<usize> {
        let frame = &self.frames[from];
        if site.local {
            if let Some(local) = self.targets.get(&(frame.scope, name.into(), arity))
                .or_else(|| self.targets.get(&(Scope::File(frame.unit), name.into(), arity))) {
                return *local;
            }
        } else if root == name { return None; }
        let resolution = self.resolve_import(frame.unit, name, arity, &mut exports::ResolutionContext::default());
        if let Some(reason) = resolution.limitation() {
            self.unresolved.borrow_mut().insert((from, site.start, reason));
        }
        resolution.unique()
    }
}

impl Associated {
    pub(super) fn index_types(&mut self, unit: usize, input: &KeyedUnit<'_, '_, '_>) {
        if let Some(function) = input.functions.first() {
            let root = std::iter::successors(Some(function.node), |node| node.parent()).last().unwrap();
            if root.kind() == "source_file" {
                signature::record_local_types(root, input.source, unit, &mut self.local_types);
            }
        }
    }

    pub(super) fn register(&mut self, unit: usize, node: Node<'_>, source: &[u8], name: &str, arity: usize, index: usize) {
        if let Some(owner) = signature::local_rust_associated_owner(node, source) {
            self.targets.entry((unit, owner, name.to_owned(), arity))
                .and_modify(|target| *target = None).or_insert(Some(index));
        }
    }

    // None means the call is not a known local associated call. Some(None)
    // denotes a local or Self call whose target is ambiguous or unsupported.
    pub(super) fn lookup(&self, unit: usize, export: Option<&Export>, name: &str, arity: usize) -> Option<Option<usize>> {
        let (owner, method) = name.split_once("::")?;
        if method.contains(':') { return None; }
        let owner = if owner == "Self" {
            let Some(owner) = export.and_then(|export| export.owner.as_deref()) else { return Some(None); };
            owner
        } else { owner };
        if let Some(target) = self.targets.get(&(unit, owner.to_owned(), method.to_owned(), arity)) {
            return Some(*target);
        }
        self.local_types.contains(&(unit, owner.to_owned())).then_some(None)
    }
}
