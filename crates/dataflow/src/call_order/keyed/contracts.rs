use crate::call_order::ContractMode;

use super::{CallSite, Engine, Entry, IdentityOperand, State, StatementLocation};

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
        if plan.source_mode(name) != Some(ContractMode::MustComplete) {
            return Some(false);
        }
        if plan.needs_completion(site) {
            let handle = state.allocate(plan, name, site.start);
            state.pending.insert(
                Entry::handle(handle.clone()),
                self.registration(frame, location.line),
            );
            state.results.insert(site.start, Some(handle));
        }
        Some(true)
    }

    pub(super) fn allow_opaque_transfer(
        &self,
        frame: usize,
        name: &str,
        site: Option<&CallSite>,
        state: &mut State<'_>,
    ) {
        let Some((plan, site)) = self.program.frames[frame].plan.as_ref().zip(site) else { return };
        if !allows_unresolved_transfer(plan, name, site, &state.values) {
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

fn allows_unresolved_transfer(
    plan: &crate::call_order::plan::Plan<'_, '_>, name: &str, site: &CallSite,
    values: &super::Values,
) -> bool {
    let Some(contract) = &plan.spec.contract else { return false };
    let method = name.rsplit('.').next().unwrap_or(name);
    contract.mode == ContractMode::MustComplete
        && !plan.is_completion(name, site.start, values)
        && !contract.borrows.iter().any(|borrow| borrow == method)
}
