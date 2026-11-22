use std::cmp::Ordering;

use crate::cfg::{BindingWrite, CfgNode};

use super::{BTreeMap, Cfg, EdgeKind, Entry, IdentityOperand, Plan, Stmt, Values};

#[derive(Clone)]
pub(super) enum Deferred<'graph> {
    Remove(Entry),
    Block(&'graph Cfg<Stmt>),
}

impl Ord for Deferred<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Remove(left), Self::Remove(right)) => left.cmp(right),
            (Self::Block(left), Self::Block(right)) => {
                std::ptr::from_ref(*left).cmp(&std::ptr::from_ref(*right))
            }
            (Self::Remove(_), Self::Block(..)) => Ordering::Less,
            (Self::Block(..), Self::Remove(_)) => Ordering::Greater,
        }
    }
}
impl PartialOrd for Deferred<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Deferred<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Deferred<'_> {}

#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct State<'graph> {
    pub(super) values: Values,
    pub(super) pending: BTreeMap<Entry, u32>,
    pub(super) deferred: Vec<Deferred<'graph>>,
    conditions: BTreeMap<IdentityOperand, bool>,
}

impl State<'_> {
    pub(super) fn rebind(&mut self, plan: &Plan<'_, '_>, write: &BindingWrite) {
        let value = write
            .value
            .as_ref()
            .and_then(|value| plan.syntax.value(&self.values, value, write.start));
        self.values.insert(write.name.clone(), value);
    }

    pub(super) fn update(&mut self, entry: Entry, registered: bool, removed: bool, line: u32) {
        if removed {
            self.pending.remove(&entry);
        } else if registered {
            self.pending.insert(entry, line);
        }
    }

    pub(super) fn branch(
        &self,
        plan: &Plan<'_, '_>,
        node: &CfgNode<Stmt>,
        edge: EdgeKind,
    ) -> Option<Self> {
        let mut state = self.clone();
        if edge == EdgeKind::Exceptional {
            let (name, start) = node.throwing_call.as_ref()?;
            return plan.may_throw(name, *start, &state.values).then_some(state);
        }
        let Some((name, positive)) = node
            .condition
            .as_ref()
            .filter(|_| matches!(edge, EdgeKind::True | EdgeKind::False))
        else {
            return Some(state);
        };
        let Some(value) = plan.syntax.resolve(&state.values, name, usize::MAX) else {
            return Some(state);
        };
        let truth = (edge == EdgeKind::True) == *positive;
        if let IdentityOperand::Boolean(value) = value {
            return (value == truth).then_some(state);
        }
        if state
            .conditions
            .get(&value)
            .is_some_and(|current| *current != truth)
        {
            return None;
        }
        if plan.syntax.repeated_guard(name) {
            state.conditions.insert(value, truth);
        }
        Some(state)
    }
}
