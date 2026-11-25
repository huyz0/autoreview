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

pub(super) struct Resolution<T: Ord = usize> {
    targets: BTreeSet<T>,
    incomplete: bool,
    limit: bool,
}

impl<T: Ord> Default for Resolution<T> {
    fn default() -> Self {
        Self { targets: BTreeSet::new(), incomplete: false, limit: false }
    }
}

impl<T: Ord> Resolution<T> {
    fn merge(&mut self, other: Self) {
        self.targets.extend(other.targets);
        self.incomplete |= other.incomplete;
        self.limit |= other.limit;
    }

    pub(super) fn unique(mut self) -> Option<T> {
        (!self.incomplete && self.targets.len() == 1).then(|| self.targets.pop_first()).flatten()
    }

    pub(super) fn limitation(&self) -> Option<&'static str> {
        if self.limit { Some("reexport_resolution_limit") }
        else if self.incomplete || self.targets.len() > 1 { Some("unresolved_helper_target") }
        else { None }
    }
}

/// Declaration-specific lookups used by the common module traversal.
/// A local lookup returns `None` only when the name is not locally bound;
/// an incomplete result represents a binding that cannot be modeled.
trait ExportTargets {
    type Target: Ord;
    fn imports(&self, unit: usize) -> &[super::KeyedImport];
    fn exports(&self, unit: usize) -> &[KeyedExport];
    fn provider(&self, path: &str) -> Option<usize>;
    fn direct(&self, unit: usize, name: &str, arity: usize) -> (bool, Resolution<Self::Target>);
    fn local(&self, unit: usize, name: &str, arity: usize) -> Option<Resolution<Self::Target>>;
    fn owned(&self, unit: usize, binding: &super::KeyedImport, name: &str, arity: usize) -> Resolution<Self::Target>;
}

struct ExportTraversal<'a, T> {
    targets: &'a T,
}

impl<T: ExportTargets> ExportTraversal<'_, T> {
    fn import(&self, unit: usize, name: &str, arity: usize, context: &mut ResolutionContext) -> Resolution<T::Target> {
        let mut result = Resolution::default();
        for binding in self.targets.imports(unit) {
            let Some(name) = binding.target(name) else { continue };
            let Some(provider) = self.targets.provider(&binding.path) else {
                result.incomplete = true;
                continue;
            };
            if binding.exported && binding.owner.is_none() {
                result.merge(self.export(provider, name, arity, context));
            } else {
                result.merge(self.targets.owned(provider, binding, name, arity));
            }
        }
        result
    }

    fn export(&self, unit: usize, name: &str, arity: usize, context: &mut ResolutionContext) -> Resolution<T::Target> {
        let key = (unit, name.to_owned());
        if context.seen.contains(&key) { return Resolution::default(); }
        if context.remaining == 0 || context.seen.len() >= EXPORT_DEPTH_LIMIT {
            return Resolution { incomplete: true, limit: true, ..Resolution::default() };
        }
        context.remaining -= 1;
        context.seen.insert(key.clone());
        let (mut explicit, mut result) = self.targets.direct(unit, name, arity);
        for export in self.targets.exports(unit) {
            if let KeyedExport::Named { exported, local, path } = export {
                if exported == name {
                    explicit = true;
                    result.merge(self.named(unit, local, path.as_deref(), arity, context));
                }
            }
        }
        if !explicit && name != "default" {
            for export in self.targets.exports(unit) {
                match export {
                    KeyedExport::Star { path } => result.merge(self.reexport(path, name, arity, context)),
                    KeyedExport::Unknown => result.incomplete = true,
                    _ => {}
                }
            }
        }
        context.seen.remove(&key);
        result
    }

    fn named(&self, unit: usize, local: &str, path: Option<&str>, arity: usize, context: &mut ResolutionContext) -> Resolution<T::Target> {
        let mut result = if let Some(path) = path {
            self.reexport(path, local, arity, context)
        } else if let Some(local) = self.targets.local(unit, local, arity) {
            return local;
        } else {
            self.import(unit, local, arity, context)
        };
        result.incomplete |= result.targets.is_empty();
        result
    }

    fn reexport(&self, path: &str, name: &str, arity: usize, context: &mut ResolutionContext) -> Resolution<T::Target> {
        match self.targets.provider(path) {
            Some(provider) => self.export(provider, name, arity, context),
            None => Resolution { incomplete: true, ..Resolution::default() },
        }
    }
}

impl Program<'_, '_, '_> {
    pub(super) fn resolve_import(
        &self, unit: usize, name: &str, arity: usize, context: &mut ResolutionContext,
    ) -> Resolution {
        ExportTraversal { targets: self }.import(unit, name, arity, context)
    }
}

impl ExportTargets for Program<'_, '_, '_> {
    type Target = usize;

    fn imports(&self, unit: usize) -> &[super::KeyedImport] { self.units[unit].imports }
    fn exports(&self, unit: usize) -> &[KeyedExport] { self.units[unit].exports }
    fn provider(&self, path: &str) -> Option<usize> { self.paths.get(path).copied() }

    fn direct(&self, unit: usize, name: &str, arity: usize) -> (bool, Resolution) {
        let mut result = Resolution::default();
        let mut explicit = false;
        for &index in &self.units[unit].frames {
            let frame = &self.frames[index];
            if frame.export.as_ref().is_some_and(|export| export.visible && export.owner.is_none()
                && (export.name == name || (name == "default" && export.default))) {
                explicit = true;
                if frame.params.len() == arity { result.targets.insert(index); }
            }
        }
        (explicit, result)
    }

    fn local(&self, unit: usize, name: &str, arity: usize) -> Option<Resolution> {
        self.targets.get(&(Scope::File(unit), name.to_owned(), arity)).map(|target| Resolution {
            targets: target.iter().copied().collect(), incomplete: target.is_none(), limit: false,
        })
    }

    fn owned(&self, unit: usize, binding: &super::KeyedImport, name: &str, arity: usize) -> Resolution {
        Resolution {
            targets: self.units[unit].frames.iter().copied()
                .filter(|index| self.frames[*index].matches_export(binding, name, arity)).collect(),
            ..Resolution::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Resolution;

    #[test]
    fn resolution_distinguishes_declaration_identities_and_incomplete_paths() {
        let target = ("src/factory.ts", 42usize);
        let mut resolution = Resolution::default();
        resolution.targets.insert(target);
        let mut duplicate = Resolution::default();
        duplicate.targets.insert(target);
        resolution.merge(duplicate);
        assert_eq!(resolution.unique(), Some(target));

        let mut ambiguous = Resolution::default();
        ambiguous.targets.insert(target);
        ambiguous.targets.insert(("src/other.ts", 42));
        assert_eq!(ambiguous.limitation(), Some("unresolved_helper_target"));
        assert_eq!(ambiguous.unique(), None);

        let mut incomplete = Resolution::default();
        incomplete.targets.insert(target);
        incomplete.merge(Resolution { incomplete: true, ..Resolution::default() });
        assert_eq!(incomplete.unique(), None);
    }
}
