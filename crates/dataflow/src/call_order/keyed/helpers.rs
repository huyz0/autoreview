use crate::cfg::{CfgNode, Stmt};

use super::{CallSite, Deferred, Engine, State, StatementLocation, Values};

type Outcomes<'graph> = Vec<(bool, State<'graph>)>;
type Results<'graph> = Vec<(bool, Option<super::IdentityOperand>, State<'graph>)>;

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
        let outcomes = self.invoke(target, values, state.clone())?;
        Some(Some(
            outcomes
                .into_iter()
                .map(|(threw, returned, mut state)| {
                    state.executed_call = (!threw).then_some(call.start);
                    state.result = Some((call.start, returned));
                    (threw, state)
                })
                .collect(),
        ))
    }

    pub(super) fn apply_call(
        &mut self,
        frame: usize,
        name: &str,
        site: Option<&CallSite>,
        state: &mut State<'graph>,
        location: StatementLocation,
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
                .map(|(threw, returned, mut state)| {
                    state.result = site.map(|site| (site.start, returned));
                    (!threw).then_some(state)
                })
                .collect::<Option<Vec<_>>>()?;
            return Some(Some(normal));
        }

        let plan = self.program.frames[frame].plan.as_ref()?;
        let call = plan.call(name, site, &state.values)?;
        if let Some(entry) = call.entry {
            if let Some(&registration) = state.pending.get(&entry).filter(|_| call.trigger) {
                self.record_hit(name, frame, location, &entry, registration);
            }
            state.update(
                entry,
                call.registered,
                call.removed,
                self.registration(frame, location.line),
            );
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
    ) -> Option<Results<'graph>> {
        if self.active.len() >= 12 || self.active.contains(&frame) {
            return None;
        }
        let caller = state.suspend(values);
        self.active.push(frame);
        let results = self.helper_exits(frame, state);
        self.active.pop();
        Some(
            results?
                .into_iter()
                .map(|(threw, mut state)| {
                    let result = state.restore(&caller);
                    (threw, result, state)
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
