use std::collections::BTreeSet;

use super::{KeyedExport, Program, Scope};

const EXPORT_STEP_LIMIT: usize = 4096;
const EXPORT_DEPTH_LIMIT: usize = 128;

pub(super) struct ResolutionContext {
    seen: BTreeSet<(usize, String)>,
    remaining: usize,
}

impl Default for ResolutionContext {
    fn default() -> Self {
        Self { seen: BTreeSet::new(), remaining: EXPORT_STEP_LIMIT }
    }
}

#[derive(Default)]
pub(super) struct Resolution {
    targets: BTreeSet<usize>,
    incomplete: bool,
    limit: bool,
}

impl Resolution {
    fn merge(&mut self, other: Self) {
        self.targets.extend(other.targets);
        self.incomplete |= other.incomplete;
        self.limit |= other.limit;
    }

    pub(super) fn unique(mut self) -> Option<usize> {
        (!self.incomplete && self.targets.len() == 1).then(|| self.targets.pop_first()).flatten()
    }

    pub(super) fn limitation(&self) -> Option<&'static str> {
        if self.limit { Some("reexport_resolution_limit") }
        else if self.incomplete || self.targets.len() > 1 { Some("unresolved_helper_target") }
        else { None }
    }
}

impl Program<'_, '_, '_> {
    pub(super) fn resolve_import(
        &self, unit: usize, name: &str, arity: usize, context: &mut ResolutionContext,
    ) -> Resolution {
        let mut result = Resolution::default();
        for binding in self.units[unit].imports {
            let Some(target) = binding.target(name) else { continue };
            let Some(&provider) = self.paths.get(binding.path.as_str()) else {
                result.incomplete = true;
                continue;
            };
            if binding.exported && binding.owner.is_none() {
                result.merge(self.resolve_export(provider, target, arity, context));
            } else {
                result.targets.extend(self.units[provider].frames.iter().copied()
                    .filter(|index| self.frames[*index].matches_export(binding, target, arity)));
            }
        }
        result
    }

    fn resolve_export(
        &self, unit: usize, name: &str, arity: usize, context: &mut ResolutionContext,
    ) -> Resolution {
        let key = (unit, name.to_owned());
        if context.seen.contains(&key) {
            return Resolution::default();
        }
        if context.remaining == 0 || context.seen.len() >= EXPORT_DEPTH_LIMIT {
            return Resolution { incomplete: true, limit: true, ..Resolution::default() };
        }
        context.remaining -= 1;
        context.seen.insert(key.clone());
        let mut result = Resolution::default();
        let mut explicit = false;
        for &index in &self.units[unit].frames {
            let frame = &self.frames[index];
            if frame.export.as_ref().is_some_and(|export| export.visible && export.owner.is_none()
                && (export.name == name || (name == "default" && export.default))) {
                explicit = true;
                if frame.params.len() == arity {
                    result.targets.insert(index);
                }
            }
        }
        for export in self.units[unit].exports {
            if let KeyedExport::Named { exported, local, path } = export {
                if exported == name {
                    explicit = true;
                    result.merge(self.resolve_named_export(unit, local, path.as_deref(), arity, context));
                }
            }
        }
        if !explicit && name != "default" {
            for export in self.units[unit].exports {
                match export {
                    KeyedExport::Star { path } => result.merge(self.resolve_reexport(path, name, arity, context)),
                    KeyedExport::Unknown => result.incomplete = true,
                    _ => {}
                }
            }
        }
        context.seen.remove(&key);
        result
    }

    fn resolve_named_export(
        &self, unit: usize, local: &str, path: Option<&str>, arity: usize,
        context: &mut ResolutionContext,
    ) -> Resolution {
        if let Some(path) = path {
            let mut result = self.resolve_reexport(path, local, arity, context);
            result.incomplete |= result.targets.is_empty();
            return result;
        }
        if let Some(target) = self.targets.get(&(Scope::File(unit), local.to_owned(), arity)) {
            return Resolution { targets: target.iter().copied().collect(), incomplete: target.is_none(), limit: false };
        }
        let mut result = self.resolve_import(unit, local, arity, context);
        // A named export exists even if it is not a modeled callable or its
        // signature differs. It must still shadow or conflict with a star.
        result.incomplete |= result.targets.is_empty();
        result
    }

    fn resolve_reexport(
        &self, path: &str, name: &str, arity: usize, context: &mut ResolutionContext,
    ) -> Resolution {
        match self.paths.get(path) {
            Some(&provider) => self.resolve_export(provider, name, arity, context),
            None => Resolution { incomplete: true, ..Resolution::default() },
        }
    }
}
