use std::collections::{BTreeMap, VecDeque};

use super::{Analysis, Body, Demand, Effect, HelperCall, Live, NodeId, Origin, Place};

impl Analysis {
    pub(super) fn opaque_origins(&self) -> Vec<(NodeId, Origin)> {
        let seed = Live::new();
        let mut found = BTreeMap::new();
        self.opaque_record(&seed, &mut |node, origin| {
            found.insert((origin.path.clone(), origin.start, origin.component), (node, origin.clone()));
        });
        found.into_values().collect()
    }

    pub(super) fn opaque_record(&self, seed: &Live, report: &mut dyn FnMut(NodeId, &Origin)) -> Live {
        let facts = solve(&self.owner, seed, &self.closures);
        record(&self.owner, &facts, seed, &self.closures, report);
        facts[self.owner.flow.entry].clone()
    }
}

fn solve(body: &Body, seed: &Live, closures: &[Body]) -> Vec<Live> {
    let mut facts = vec![Live::new(); body.effects.len()];
    let mut queued = body.flow.reachable.clone();
    let mut work: VecDeque<_> = (0..body.effects.len()).rev().filter(|&node| queued[node]).collect();
    while let Some(node) = work.pop_front() {
        queued[node] = false;
        let mut live = body.flow.live_after(node, &facts, seed);
        transfer(&body.effects[node], &mut live, closures, None);
        if facts[node] != live {
            facts[node] = live;
            for &before in &body.flow.predecessors[node] {
                if !std::mem::replace(&mut queued[before], true) { work.push_back(before); }
            }
        }
    }
    facts
}

fn record(body: &Body, facts: &[Live], seed: &Live, closures: &[Body], report: &mut dyn FnMut(NodeId, &Origin)) {
    for (node, effects) in body.effects.iter().enumerate().filter(|(node, _)| body.flow.reachable[*node]) {
        let mut live = body.flow.live_after(node, facts, seed);
        transfer(effects, &mut live, closures, Some(&mut |origin| report(node, origin)));
    }
}

type Observer<'a> = dyn FnMut(&Origin) + 'a;

fn transfer(effects: &[Effect], live: &mut Live, closures: &[Body], mut report: Option<&mut Observer<'_>>) {
    for effect in effects.iter().rev() {
        match effect {
            Effect::OpaqueRead(place) => { live.insert(*place); }
            Effect::OpaqueOnly(nested) => transfer(nested, live, closures, report.as_deref_mut()),
            Effect::ObservedOnly(_) => {}
            // A known use before an opaque call settles that path. Other paths
            // retain their uncertainty when the control-flow facts are joined.
            Effect::Read(place) => { live.remove(place); }
            Effect::Write(writes) => opaque_writes(writes, live, report.as_deref_mut()),
            Effect::ApplyHelper(call) => helper(call, live, report.as_deref_mut()),
            Effect::Invoke(index, demand) => invoke(&closures[*index], *demand, live, report.as_deref_mut()),
        }
    }
}

fn opaque_writes(writes: &[super::Write], live: &mut Live, mut report: Option<&mut Observer<'_>>) {
    let mut copies = Vec::new();
    for write in writes.iter().rev() {
        let used = write.target.is_some_and(|place| live.remove(&place));
        let mut pending = vec![&write.value];
        while let Some(value) = pending.pop() {
            match value {
                super::Value::Copy(place) if used => copies.push(*place),
                super::Value::Source(origin) if used => { if let Some(report) = &mut report { report(origin); } }
                super::Value::Aggregate(items) if used => pending.extend(items),
                super::Value::StagedAggregate(elements) if used => copies.extend(elements.iter().copied()),
                _ => {}
            }
        }
    }
    live.extend(copies);
}

fn helper(call: &HelperCall, live: &mut Live, mut report: Option<&mut Observer<'_>>) {
    let unknown_return = matches!(call.demand, Demand::Slot(slot) if live.remove(&slot));
    let parameters = if unknown_return { &call.opaque_observed } else { &call.opaque_discarded };
    for (argument, opaque) in call.arguments.iter().zip(parameters) {
        if *opaque { live.extend(*argument); }
    }
    if let Some(report) = &mut report {
        let origins = if unknown_return { &call.opaque_origins_observed } else { &call.opaque_origins_discarded };
        for origin in origins { report(origin); }
    }
}

fn invoke(body: &Body, demand: Demand, live: &mut Live, mut report: Option<&mut Observer<'_>>) {
    let mut seed = std::mem::take(live);
    if matches!(demand, Demand::Slot(slot) if seed.remove(&slot)) { seed.insert(Place::Return); }
    let facts = solve(body, &seed, &[]);
    live.clone_from(&facts[body.flow.entry]);
    live.remove(&Place::Return);
    if let Some(report) = &mut report { record(body, &facts, &seed, &[], &mut |_, origin| report(origin)); }
}
