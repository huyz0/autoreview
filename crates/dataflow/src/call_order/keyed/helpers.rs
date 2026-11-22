use crate::cfg::{CfgNode, Stmt};

use super::{hit, CallSite, Deferred, Engine, NodeId, State, Values};

type Outcomes<'graph> = Vec<(bool, State<'graph>)>;

impl<'graph> Engine<'_, '_, '_, 'graph> {
    pub(super) fn fork_helper(
        &mut self,
        frame: usize,
        node: &CfgNode<Stmt>,
        state: &State<'graph>,
    ) -> Option<Option<Outcomes<'graph>>> {
        let Some(call) = &node.throwing_call else {
            return Some(None);
        };
        let Some((target, values)) = self.helper(frame, &call.name, call.site.as_deref(), state)
        else {
            return Some(None);
        };
        let mut outcomes = self.invoke(target, values, state.clone())?;
        for (threw, state) in &mut outcomes {
            state.executed_call = (!*threw).then_some(call.start);
        }
        Some(Some(outcomes))
    }

    pub(super) fn apply_call(
        &mut self,
        frame: usize,
        name: &str,
        site: Option<&CallSite>,
        state: &mut State<'graph>,
        id: NodeId,
        line: u32,
    ) -> Option<Option<Vec<State<'graph>>>> {
        if state
            .executed_call
            .take()
            .is_some_and(|start| site.is_some_and(|site| site.start == start))
        {
            return Some(None);
        }
        if let Some((target, arguments)) = self.helper(frame, name, site, state) {
            let outcomes = self.invoke(target, arguments, state.clone())?;
            let normal = outcomes
                .into_iter()
                .map(|(threw, state)| (!threw).then_some(state))
                .collect::<Option<Vec<_>>>()?;
            return Some(Some(normal));
        }

        let plan = self.program.frames[frame].plan.as_ref()?;
        let call = plan.call(name, site, &state.values)?;
        if let Some(entry) = call.entry {
            if let Some(&registration) = state.pending.get(&entry).filter(|_| call.trigger) {
                self.hits.push(hit(name, id, line, &entry, registration));
            }
            state.update(entry, call.registered, call.removed, line);
        }
        Some(None)
    }

    pub(super) fn schedule_helper(
        &self,
        frame: usize,
        name: &str,
        site: Option<&CallSite>,
        state: &mut State<'graph>,
    ) -> Option<()> {
        if let Some((target, arguments)) = self.helper(frame, name, site, state) {
            return state.schedule(Deferred::Helper(target, arguments));
        }
        let plan = self.program.frames[frame].plan.as_ref()?;
        let call = plan.call(name, site, &state.values)?;
        if call.registered || call.trigger {
            return None;
        }
        if let Some(entry) = call.entry.filter(|_| call.removed) {
            state.schedule(Deferred::Remove(entry))?;
        }
        Some(())
    }

    fn helper(
        &self,
        frame: usize,
        name: &str,
        site: Option<&CallSite>,
        state: &State<'_>,
    ) -> Option<(usize, Values)> {
        let site = site?;
        let plan = self.program.frames[frame].plan.as_ref()?;
        if !plan.may_throw(name, site.start, &state.values) {
            return None;
        }
        let index = self.program.target(frame, name, site)?;
        let arguments = site
            .arguments
            .iter()
            .map(|value| {
                plan.syntax
                    .value(&state.values, value.as_ref()?, site.start)
            })
            .collect();
        Some((index, self.program.frames[index].bind(arguments)))
    }

    pub(super) fn invoke(
        &mut self,
        frame: usize,
        values: Values,
        mut state: State<'graph>,
    ) -> Option<Outcomes<'graph>> {
        if self.active.len() >= 12 || self.active.contains(&frame) {
            return None;
        }
        let caller_values = std::mem::replace(&mut state.values, values);
        let caller_deferred = std::mem::take(&mut state.deferred);
        let caller_call = state.executed_call.take();
        self.active.push(frame);
        let results = self.helper_exits(frame, state);
        self.active.pop();
        Some(
            results?
                .into_iter()
                .map(|(threw, mut state)| {
                    state.values = caller_values.clone();
                    state.deferred = caller_deferred.clone();
                    state.executed_call = caller_call;
                    (threw, state)
                })
                .collect(),
        )
    }

    fn helper_exits(&mut self, frame: usize, state: State<'graph>) -> Option<Outcomes<'graph>> {
        let graph = self.program.frames[frame].graph;
        let mut outcomes = Vec::new();
        for exit in self.paths(frame, graph, state)? {
            let threw = matches!(exit.trigger, "throw" | "implicit throw");
            outcomes.extend(
                self.unwind(frame, exit.state)?
                    .into_iter()
                    .map(|state| (threw, state)),
            );
        }
        Some(outcomes)
    }
}
