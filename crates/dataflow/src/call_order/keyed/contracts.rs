use crate::call_order::ContractMode;

use super::{CallOrderHit, CallSite, Engine, Entry, IdentityOperand, State, StatementLocation};
use crate::call_order::plan::Call;

impl Engine<'_, '_, '_, '_> {
    pub(super) fn apply_contract_source(
        &mut self,
        frame: usize,
        name: &str,
        site: &CallSite,
        state: &mut State<'_>,
        location: StatementLocation,
    ) -> Option<bool> {
        let plan = self.program.frames[frame].plan.as_ref()?;
        match plan.source_mode(name) {
            None => return Some(false),
            Some(ContractMode::MustUse) if site.discarded => {
                self.hits.push(CallOrderHit {
                    trigger_call: name.to_owned(),
                    node: location.node,
                    source_line: location.line,
                    path: self.program.path(self.program.frames[frame].unit),
                    entry: None,
                });
            }
            Some(ContractMode::MustUse) => {}
            Some(ContractMode::MustComplete) => {
                if !plan.needs_completion(site) {
                    return Some(true);
                }
                let handle = state.allocate(plan, name, site.start);
                state.pending.insert(
                    Entry::handle(handle.clone()),
                    self.registration(frame, location.line),
                );
                state.result = Some((site.start, Some(handle)));
            }
        }
        Some(true)
    }

    pub(super) fn allow_opaque_transfer(
        &self,
        frame: usize,
        name: &str,
        site: Option<&CallSite>,
        call: &Call,
        state: &mut State<'_>,
    ) {
        let Some(plan) = self.program.frames[frame].plan.as_ref() else {
            return;
        };
        let Some(contract) = &plan.spec.contract else {
            return;
        };
        let Some(site) =
            site.filter(|_| !call.removed && contract.mode == ContractMode::MustComplete)
        else {
            return;
        };
        if plan.is_completion(name, site.start, &state.values) {
            return;
        }
        let method = name.rsplit('.').next().unwrap_or(name);
        if contract.borrows.iter().any(|borrow| borrow == method) {
            return;
        }
        let receiver = site
            .receiver
            .as_ref()
            .map(|receiver| IdentityOperand::Binding(receiver.clone()));
        for operand in site.arguments.iter().flatten().chain(receiver.iter()) {
            if let Some(handle) = state.operand(plan, operand, site.start) {
                state.remove(&Entry::handle(handle));
            }
        }
    }
}
