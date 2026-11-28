use super::{Bindings, IdentityOperand};
use std::collections::HashSet;

impl<'src> Bindings<'src> {
    pub(in crate::call_order) fn stable_identity<'a>(
        &'a self,
        mut name: &'a str,
        mut at: usize,
    ) -> Option<IdentityOperand> {
        let mut seen = HashSet::new();
        while seen.len() < 64 && seen.insert(name) {
            (self.can_read(name) && !self.all_writes.contains(name)).then_some(())?;
            let binding = self.declarations.get(name)?.as_ref()?;
            (binding.visible_at <= at).then_some(())?;
            let Some(alias) = binding.alias.as_ref() else {
                return Some(IdentityOperand::Binding(name.to_owned()));
            };
            alias.borrowed_root.is_none().then_some(())?;
            match &alias.operand {
                IdentityOperand::Binding(origin) => {
                    name = origin;
                    at = alias.captured_at;
                }
                value => return Some(value.clone()),
            }
        }
        None
    }

    pub(in crate::call_order) fn stable_boolean(&self, name: &str, at: usize) -> Option<bool> {
        match self.stable_identity(name, at)? {
            IdentityOperand::Boolean(value) => Some(value),
            _ => None,
        }
    }
}
