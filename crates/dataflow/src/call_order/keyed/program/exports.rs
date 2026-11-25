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
        let (explicit, mut result) = self.explicit(unit, name, arity, context);
        if !explicit && name != "default" {
            result.merge(self.stars(unit, name, arity, context));
        }
        context.seen.remove(&key);
        result
    }

    fn explicit(&self, unit: usize, name: &str, arity: usize, context: &mut ResolutionContext) -> (bool, Resolution<T::Target>) {
        let (mut explicit, mut result) = self.targets.direct(unit, name, arity);
        for export in self.targets.exports(unit) {
            if let KeyedExport::Named { exported, local, path } = export {
                if exported == name {
                    explicit = true;
                    result.merge(self.named(unit, local, path.as_deref(), arity, context));
                }
            }
        }
        (explicit, result)
    }

    fn stars(&self, unit: usize, name: &str, arity: usize, context: &mut ResolutionContext) -> Resolution<T::Target> {
        let mut result = Resolution::default();
        for export in self.targets.exports(unit) {
            match export {
                KeyedExport::Star { path } => result.merge(self.reexport(path, name, arity, context)),
                KeyedExport::Unknown => result.incomplete = true,
                _ => {}
            }
        }
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

/// A module-local callable binding and its accepted argument counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyedDeclaration {
    pub unit: usize,
    pub name: String,
    pub start: usize,
    pub minimum: usize,
    pub maximum: usize,
}

/// The unique declaration index, or a reason why resolution was incomplete.
pub struct DeclarationResolution {
    pub declaration: Option<usize>,
    pub limitation: Option<&'static str>,
}

struct DeclarationTargets<'a, 'src, 'tree, 'graph> {
    units: &'a [super::KeyedUnit<'src, 'tree, 'graph>],
    declarations: &'a [KeyedDeclaration],
}

impl ExportTargets for DeclarationTargets<'_, '_, '_, '_> {
    type Target = usize;
    fn imports(&self, unit: usize) -> &[super::KeyedImport] { self.units[unit].imports }
    fn exports(&self, unit: usize) -> &[KeyedExport] { self.units[unit].exports }
    fn provider(&self, path: &str) -> Option<usize> {
        let mut matches = self.units.iter().enumerate().filter(|(_, unit)| unit.path == path);
        let (index, _) = matches.next()?;
        matches.next().is_none().then_some(index)
    }
    fn direct(&self, _: usize, _: &str, _: usize) -> (bool, Resolution) { (false, Resolution::default()) }
    fn local(&self, unit: usize, name: &str, arity: usize) -> Option<Resolution> {
        let matches: Vec<_> = self.declarations.iter().enumerate()
            .filter(|(_, declaration)| declaration.unit == unit && declaration.name == name).collect();
        if matches.is_empty() { return None; }
        let mut result = Resolution::default();
        if matches.len() == 1 && (matches[0].1.minimum..=matches[0].1.maximum).contains(&arity) {
            result.targets.insert(matches[0].0);
        } else {
            result.incomplete = true;
        }
        Some(result)
    }
    fn owned(&self, _: usize, _: &super::KeyedImport, _: &str, _: usize) -> Resolution {
        Resolution { incomplete: true, ..Resolution::default() }
    }
}

/// Resolve an imported callable declaration through the verified module graph.
/// The caller supplies all relevant local bindings, including untyped ones;
/// omitted bindings can otherwise conceal shadowing or ambiguity.
pub fn resolve_declaration_import(
    units: &[super::KeyedUnit<'_, '_, '_>], declarations: &[KeyedDeclaration], unit: usize, name: &str, arity: usize,
) -> DeclarationResolution {
    if unit >= units.len() {
        return DeclarationResolution { declaration: None, limitation: Some("unresolved_helper_target") };
    }
    let targets = DeclarationTargets { units, declarations };
    let result = ExportTraversal { targets: &targets }.import(unit, name, arity, &mut ResolutionContext::default());
    let limitation = result.limitation();
    DeclarationResolution { declaration: result.unique(), limitation }
}

#[cfg(test)]
mod tests {
    use super::{resolve_declaration_import, ExportTargets, ExportTraversal, KeyedDeclaration, KeyedExport, Resolution, ResolutionContext};
    use super::super::{KeyedImport, KeyedUnit};

    struct Declarations {
        imports: Vec<Vec<KeyedImport>>,
        exports: Vec<Vec<KeyedExport>>,
        names: Vec<&'static str>,
        locals: Vec<Vec<(&'static str, usize)>>,
    }

    impl ExportTargets for Declarations {
        type Target = (usize, usize);
        fn imports(&self, unit: usize) -> &[KeyedImport] { &self.imports[unit] }
        fn exports(&self, unit: usize) -> &[KeyedExport] { &self.exports[unit] }
        fn provider(&self, path: &str) -> Option<usize> { self.names.iter().position(|name| *name == path) }
        fn direct(&self, _: usize, _: &str, _: usize) -> (bool, Resolution<Self::Target>) { (false, Resolution::default()) }
        fn local(&self, unit: usize, name: &str, _: usize) -> Option<Resolution<Self::Target>> {
            self.locals[unit].iter().find(|(local, _)| *local == name).map(|(_, offset)| {
                let mut result = Resolution::default();
                result.targets.insert((unit, *offset));
                result
            })
        }
        fn owned(&self, _: usize, _: &KeyedImport, _: &str, _: usize) -> Resolution<Self::Target> {
            Resolution { incomplete: true, ..Resolution::default() }
        }
    }

    fn named(exported: &str, local: &str, path: Option<&str>) -> KeyedExport {
        KeyedExport::Named { exported: exported.into(), local: local.into(), path: path.map(str::to_owned) }
    }

    #[test]
    fn declaration_identity_follows_barrels_and_preserves_conflicts() {
        let mut graph = Declarations {
            names: vec!["caller", "barrel", "factory", "other"],
            imports: vec![vec![KeyedImport { local: "useIt".into(), path: "barrel".into(), name: Some("upload".into()), owner: None, exported: true }], vec![], vec![], vec![]],
            exports: vec![vec![], vec![KeyedExport::Star { path: "factory".into() }], vec![named("upload", "binding", None)], vec![named("upload", "otherBinding", None)]],
            locals: vec![vec![], vec![], vec![("binding", 42)], vec![("otherBinding", 19)]],
        };
        let resolve = |graph: &Declarations| ExportTraversal { targets: graph }.import(0, "useIt", 1, &mut ResolutionContext::default());
        assert_eq!(resolve(&graph).unique(), Some((2, 42)));
        graph.exports[1].push(KeyedExport::Star { path: "other".into() });
        assert_eq!(resolve(&graph).limitation(), Some("unresolved_helper_target"));
        graph.exports[1].push(named("upload", "upload", Some("factory")));
        assert_eq!(resolve(&graph).unique(), Some((2, 42)));
        graph.exports[1].push(named("upload", "missing", Some("absent")));
        assert!(resolve(&graph).unique().is_none());
    }

    #[test]
    fn declaration_adapter_resolves_actual_units_and_optional_arity() {
        let imports = [KeyedImport { local: "run".into(), path: "barrel".into(), name: Some("upload".into()), owner: None, exported: true }];
        let barrel = [KeyedExport::Star { path: "factory".into() }];
        let factory = [named("upload", "binding", None)];
        let units = [
            KeyedUnit { path: "caller", source: b"", functions: &[], imports: &imports, exports: &[], entry: true },
            KeyedUnit { path: "barrel", source: b"", functions: &[], imports: &[], exports: &barrel, entry: false },
            KeyedUnit { path: "factory", source: b"", functions: &[], imports: &[], exports: &factory, entry: false },
        ];
        let declaration = KeyedDeclaration { unit: 2, name: "binding".into(), start: 42, minimum: 1, maximum: 2 };
        assert_eq!(resolve_declaration_import(&units, std::slice::from_ref(&declaration), 0, "run", 1).declaration, Some(0));
        assert_eq!(resolve_declaration_import(&units, std::slice::from_ref(&declaration), 0, "run", 2).declaration, Some(0));
        assert!(resolve_declaration_import(&units, std::slice::from_ref(&declaration), 0, "run", 3).declaration.is_none());
        assert!(resolve_declaration_import(&units, &[declaration.clone(), declaration], 0, "run", 1).declaration.is_none());
    }

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
