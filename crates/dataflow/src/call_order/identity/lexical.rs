use super::{IdentityOperand, Syntax};

impl Syntax<'_> {
    pub(in crate::call_order) fn taken_name_at(&self, name: &str, at: usize) -> bool {
        self.taken_name(name) || self.bindings.conditional_binding(name, at, false).is_some()
    }

    pub(in crate::call_order) fn conditional_key(
        &self,
        name: &str,
        at: usize,
        defining: bool,
    ) -> Option<String> {
        let start = self.bindings.conditional_binding(name, at, defining)?;
        Some(format!("{name}#{start}"))
    }

    pub(in crate::call_order) fn stable_boolean(&self, name: &str, at: usize) -> Option<bool> {
        if self.bindings.conditional_binding(name, at, false).is_some() {
            return None;
        }
        self.bindings.stable_boolean(name, at)
    }

    pub(in crate::call_order) fn stable_identity(
        &self,
        name: &str,
        at: usize,
    ) -> Option<IdentityOperand> {
        if self.bindings.conditional_binding(name, at, false).is_some() {
            return None;
        }
        match self.bindings.stable_identity(name, at)? {
            IdentityOperand::Binding(name) => Some(IdentityOperand::Scoped {
                name,
                owner: self.owner,
                unit: self.unit,
            }),
            value => Some(value),
        }
    }
}
