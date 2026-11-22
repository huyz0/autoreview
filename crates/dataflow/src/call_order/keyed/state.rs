use std::cmp::Ordering;

use crate::cfg::{BindingWrite, CfgNode};

use super::{BTreeMap, Cfg, EdgeKind, Entry, IdentityOperand, Plan, Stmt, Values};

#[derive(Clone)]
pub(super) enum Deferred<'graph> {
    Remove(Entry),
    Block(&'graph Cfg<Stmt>),
    Helper(usize, Values),
}

impl Ord for Deferred<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Remove(left), Self::Remove(right)) => left.cmp(right),
            (Self::Block(left), Self::Block(right)) => {
                std::ptr::from_ref(*left).cmp(&std::ptr::from_ref(*right))
            }
            (Self::Helper(left, args), Self::Helper(right, other_args)) => {
                (left, args).cmp(&(right, other_args))
            }
            _ => self.rank().cmp(&other.rank()),
        }
    }
}
impl Deferred<'_> {
    fn rank(&self) -> u8 {
        match self {
            Self::Remove(_) => 0,
            Self::Block(_) => 1,
            Self::Helper(..) => 2,
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
    pub(super) executed_call: Option<usize>,
}

impl<'graph> State<'graph> {
    pub(super) fn rebind(&mut self, plan: &Plan<'_, '_>, write: &BindingWrite) {
        if let Some(IdentityOperand::Fresh { start, .. }) = &write.value {
            self.age(*start);
        }
        let value = write
            .value
            .as_ref()
            .and_then(|value| plan.syntax.value(&self.values, value, write.start));
        self.values.insert(write.name.clone(), value);
    }

    // Two most recent values stay distinct. Older iterations form a
    // summary that a single removal cannot discharge.
    fn age(&mut self, site: usize) {
        for value in self.values.values_mut().flatten() {
            advance(value, site);
        }
        let mut pending = BTreeMap::new();
        for (mut entry, line) in std::mem::take(&mut self.pending) {
            advance_entry(&mut entry, site);
            pending
                .entry(entry)
                .and_modify(|previous: &mut u32| *previous = (*previous).min(line))
                .or_insert(line);
        }
        self.pending = pending;
        for action in &mut self.deferred {
            match action {
                Deferred::Remove(entry) => advance_entry(entry, site),
                Deferred::Helper(_, values) => {
                    for value in values.values_mut().flatten() {
                        advance(value, site);
                    }
                }
                Deferred::Block(_) => {}
            }
        }
        self.conditions.retain(
            |value, _| !matches!(value, IdentityOperand::Fresh { start, .. } if *start == site),
        );
    }

    pub(super) fn schedule(&mut self, action: Deferred<'graph>) -> Option<()> {
        // Consecutive identical removals are idempotent, even after a
        // back edge. Keep order around other deferred actions.
        if let Deferred::Remove(entry) = &action {
            if self
                .deferred
                .iter()
                .rev()
                .take_while(|item| matches!(item, Deferred::Remove(_)))
                .any(|item| matches!(item, Deferred::Remove(previous) if previous == entry))
            {
                return Some(());
            }
        }
        if self.deferred.len() >= 64 {
            return None;
        }
        self.deferred.push(action);
        Some(())
    }

    pub(super) fn update(&mut self, entry: Entry, registered: bool, removed: bool, line: u32) {
        if removed {
            self.remove(&entry);
        } else if registered {
            self.pending.insert(entry, line);
        }
    }

    pub(super) fn remove(&mut self, entry: &Entry) {
        if !older(&entry.collection) && !older(&entry.key) {
            self.pending.remove(entry);
        }
    }

    pub(super) fn branch(
        &self,
        plan: &Plan<'_, '_>,
        node: &CfgNode<Stmt>,
        edge: EdgeKind,
    ) -> Option<Self> {
        match edge {
            EdgeKind::Exceptional => {
                let call = node.throwing_call.as_ref()?;
                plan.may_throw(&call.name, call.start, &self.values)
                    .then(|| self.clone())
            }
            EdgeKind::True | EdgeKind::False => {
                let mut state = self.clone();
                state.assume(plan, node, edge == EdgeKind::True)?;
                Some(state)
            }
            _ => Some(self.clone()),
        }
    }

    fn assume(&mut self, plan: &Plan<'_, '_>, node: &CfgNode<Stmt>, arm: bool) -> Option<()> {
        let Some((name, positive)) = &node.condition else {
            return Some(());
        };
        let truth = arm == *positive;
        if matches!(name.as_str(), "true" | "false") {
            return ((name == "true") == truth).then_some(());
        }
        let Some(value) = plan.syntax.resolve(&self.values, name, usize::MAX) else {
            return Some(());
        };
        if let IdentityOperand::Boolean(value) = value {
            return (value == truth).then_some(());
        }
        if node.loop_condition {
            return Some(());
        }
        if self
            .conditions
            .get(&value)
            .is_some_and(|current| *current != truth)
        {
            return None;
        }
        if plan.syntax.repeated_guard(name) {
            self.conditions.insert(value, truth);
        }
        Some(())
    }
}

fn advance_entry(entry: &mut Entry, site: usize) {
    advance(&mut entry.collection, site);
    advance(&mut entry.key, site);
}

fn advance(value: &mut IdentityOperand, site: usize) {
    if let IdentityOperand::Fresh { start, age, .. } = value {
        if *start == site {
            *age = (*age + 1).min(2);
        }
    }
}

fn older(value: &IdentityOperand) -> bool {
    matches!(value, IdentityOperand::Fresh { age: 2, .. })
}
