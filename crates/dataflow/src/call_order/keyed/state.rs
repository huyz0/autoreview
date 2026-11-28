use std::cmp::Ordering;

use crate::cfg::{BindingWrite, CfgNode};

use super::{BTreeMap, CallSite, Cfg, EdgeKind, Entry, IdentityOperand, Plan, Stmt, Values};

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
    pub(super) pending: BTreeMap<Entry, Registration>,
    pub(super) deferred: Vec<Deferred<'graph>>,
    conditions: BTreeMap<IdentityOperand, bool>,
    pub(super) executed_call: Option<usize>,
    pub(super) results: BTreeMap<usize, Option<IdentityOperand>>,
    pub(super) returned: Option<IdentityOperand>,
    allocations: Allocations,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Registration {
    pub(super) unit: usize,
    pub(super) line: u32,
}

type Allocations = BTreeMap<(usize, usize), u8>;

pub(super) struct Suspended<'graph> {
    values: Values,
    deferred: Vec<Deferred<'graph>>,
    executed_call: Option<usize>,
    results: BTreeMap<usize, Option<IdentityOperand>>,
    returned: Option<IdentityOperand>,
    allocations: Allocations,
}

impl<'graph> State<'graph> {
    pub(super) fn resolve_call_arguments(&self, plan: &Plan<'_, '_>, site: &CallSite) -> CallSite {
        let mut resolved = site.clone();
        for argument in &mut resolved.arguments {
            if let Some(value @ IdentityOperand::CallResult { .. }) = argument.as_ref() {
                *argument = self.operand(plan, value, site.start);
            }
        }
        resolved
    }

    pub(super) fn rebind(&mut self, plan: &Plan<'_, '_>, write: &BindingWrite) {
        let value = match &write.value {
            Some(IdentityOperand::CallResult {
                start,
                fallback: true,
            }) if !self.results.contains_key(start) => return,
            Some(operand) => self.materialize(plan, operand, write.start),
            None => None,
        };
        let key = plan.syntax.conditional_key(&write.name, write.start, true)
            .unwrap_or_else(|| write.name.clone());
        self.values.insert(key, value);
    }

    pub(super) fn materialize(
        &mut self,
        plan: &Plan<'_, '_>,
        operand: &IdentityOperand,
        location: usize,
    ) -> Option<IdentityOperand> {
        if let IdentityOperand::Fresh { start, name, .. } = operand {
            if let Some(Some(value)) = self.results.get(start) {
                return Some(value.clone());
            }
            return Some(self.allocate(plan, name, *start));
        }
        self.operand(plan, operand, location)
    }

    pub(super) fn allocate(&mut self, plan: &Plan<'_, '_>, name: &str, start: usize) -> IdentityOperand {
        let unit = plan.syntax.unit;
        self.age(unit, start);
        IdentityOperand::Fresh { name: name.to_owned(), start, age: 0, unit }
    }

    pub(super) fn operand(
        &self,
        plan: &Plan<'_, '_>,
        value: &IdentityOperand,
        location: usize,
    ) -> Option<IdentityOperand> {
        if let IdentityOperand::CallResult { start, .. } = value {
            return self.results.get(start).cloned().flatten();
        }
        plan.syntax.value(&self.values, value, location)
    }

    pub(super) fn suspend(&mut self, parameters: Values) -> Suspended<'graph> {
        Suspended {
            values: std::mem::replace(&mut self.values, parameters),
            deferred: std::mem::take(&mut self.deferred),
            executed_call: self.executed_call.take(),
            results: std::mem::take(&mut self.results),
            returned: self.returned.take(),
            allocations: std::mem::take(&mut self.allocations),
        }
    }

    pub(super) fn restore(&mut self, caller: &Suspended<'graph>) -> Option<IdentityOperand> {
        let returned = std::mem::replace(&mut self.returned, caller.returned.clone());
        let allocations = std::mem::replace(&mut self.allocations, caller.allocations.clone());
        self.values = caller.values.clone();
        self.deferred = caller.deferred.clone();
        self.executed_call = caller.executed_call;
        self.results = caller.results.clone();
        for ((unit, site), count) in allocations {
            self.record_generations(unit, site, count);
            for _ in 0..count {
                self.age_captures(unit, site);
            }
        }
        returned
    }

    pub(super) fn transfer_returned_handle(&mut self) {
        if let Some(handle) = &self.returned {
            self.remove(&Entry::handle(handle.clone()));
        }
    }

    fn record_generations(&mut self, unit: usize, site: usize, more: u8) {
        let count = self.allocations.entry((unit, site)).or_default();
        *count = (*count + more).min(2);
    }

    // Two most recent values stay distinct. Older iterations form a
    // summary that a single removal cannot discharge.
    fn age(&mut self, unit: usize, site: usize) {
        self.record_generations(unit, site, 1);
        self.age_captures(unit, site);
        let mut pending = BTreeMap::new();
        for (mut entry, line) in std::mem::take(&mut self.pending) {
            advance_entry(&mut entry, unit, site);
            pending
                .entry(entry)
                .and_modify(|previous: &mut Registration| *previous = (*previous).min(line))
                .or_insert(line);
        }
        self.pending = pending;
        self.conditions.retain(|value, _| !matches!(value, IdentityOperand::Fresh { start, unit: owner, .. } if *start == site && *owner == unit));
    }

    fn age_captures(&mut self, unit: usize, site: usize) {
        for value in self
            .values
            .values_mut()
            .flatten()
            .chain(self.returned.iter_mut())
        {
            advance(value, unit, site);
        }
        for value in self.results.values_mut().flatten() {
            advance(value, unit, site);
        }
        for action in &mut self.deferred {
            match action {
                Deferred::Remove(entry) => advance_entry(entry, unit, site),
                Deferred::Helper(_, values) => {
                    for value in values.values_mut().flatten() {
                        advance(value, unit, site);
                    }
                }
                Deferred::Block(_) => {}
            }
        }
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

    pub(super) fn update(
        &mut self,
        entry: Entry,
        registered: bool,
        removed: bool,
        registration: Registration,
    ) {
        if removed {
            self.remove(&entry);
        } else if registered {
            self.pending.insert(entry, registration);
        }
    }

    pub(super) fn remove(&mut self, entry: &Entry) {
        if !older(&entry.collection) && !entry.key.as_ref().is_some_and(older) {
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
        let value = match name.as_str() {
            "true" => Some(IdentityOperand::Boolean(true)),
            "false" => Some(IdentityOperand::Boolean(false)),
            _ => plan.syntax.resolve(&self.values, name, usize::MAX),
        };
        match value {
            Some(IdentityOperand::Boolean(value)) => (value == truth).then_some(()),
            Some(value) if !node.loop_condition => {
                self.record_condition(value, truth, plan.syntax.repeated_guard(name))
            }
            _ => Some(()),
        }
    }

    fn record_condition(
        &mut self,
        value: IdentityOperand,
        truth: bool,
        remember: bool,
    ) -> Option<()> {
        if self
            .conditions
            .get(&value)
            .is_some_and(|current| *current != truth)
        {
            return None;
        }
        if remember {
            self.conditions.insert(value, truth);
        }
        Some(())
    }
}

fn advance_entry(entry: &mut Entry, unit: usize, site: usize) {
    advance(&mut entry.collection, unit, site);
    if let Some(key) = &mut entry.key {
        advance(key, unit, site);
    }
}

fn advance(value: &mut IdentityOperand, unit: usize, site: usize) {
    if let IdentityOperand::Fresh {
        start,
        age,
        unit: owner,
        ..
    } = value
    {
        if *start == site && *owner == unit {
            *age = (*age + 1).min(2);
        }
    }
}

fn older(value: &IdentityOperand) -> bool {
    matches!(value, IdentityOperand::Fresh { age: 2, .. })
}
