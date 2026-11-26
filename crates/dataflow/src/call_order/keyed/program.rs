use std::collections::{BTreeMap, BTreeSet};
use std::cell::RefCell;
use tree_sitter::Node;

use crate::cfg::{CallSite, CallTarget, Cfg, IdentityOperand, Stmt};
mod signature;
mod associated;
use associated::Associated;
pub use signature::has_attributes as has_rust_attributes;
mod exports;
pub use exports::{resolve_declaration_import, DeclarationResolution, KeyedDeclaration};
use signature::{candidate, export, parameters, Export};

use super::{CallOrderSpec, Plan, Values};

/// A parsed function and the graph used by keyed cleanup rules.
pub struct KeyedFunction<'tree, 'graph> {
    pub node: Node<'tree>,
    pub graph: &'graph Cfg<Stmt>,
}

/// A source file, its lowered functions, and the imports resolved by the caller.
pub struct KeyedUnit<'src, 'tree, 'graph> {
    pub path: &'src str,
    pub source: &'src [u8],
    pub functions: &'graph [KeyedFunction<'tree, 'graph>],
    pub imports: &'graph [KeyedImport],
    pub exports: &'graph [KeyedExport],
    pub entry: bool,
}

/// A named export (local or from another unit), or a wildcard re-export.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyedExport {
    Named { exported: String, local: String, path: Option<String> },
    Star { path: String },
    /// An export whose runtime names cannot be enumerated safely.
    Unknown,
}

/// Exact local name or namespace prefix mapped to a repository source file.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyedImport {
    pub local: String,
    pub path: String,
    /// None selects a member by stripping the local namespace prefix.
    pub name: Option<String>,
    pub owner: Option<String>,
    pub exported: bool,
}

pub(in crate::call_order) struct Program<'spec, 'src, 'graph> {
    pub frames: Vec<Frame<'spec, 'src, 'graph>>,
    targets: BTreeMap<(Scope, String, usize), Option<usize>>,
    associated: Associated,
    units: Vec<Unit<'src, 'graph>>,
    paths: BTreeMap<&'src str, usize>,
    unresolved: RefCell<BTreeSet<(usize, usize, &'static str)>>,
}

pub(in crate::call_order) struct Frame<'spec, 'src, 'graph> {
    pub graph: &'graph Cfg<Stmt>,
    pub unit: usize,
    export: Option<Export>,
    pub plan: Option<Plan<'spec, 'src>>,
    scope: Scope,
    unsupported_rust_line: Option<u32>,
    pub(in crate::call_order) params: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
    File(usize),
    Class(usize, usize),
}

struct Unit<'src, 'graph> {
    path: &'src str,
    source: &'src [u8],
    imports: &'graph [KeyedImport],
    exports: &'graph [KeyedExport],
    entry: bool,
    frames: Vec<usize>,
}

impl<'spec, 'src, 'graph> Program<'spec, 'src, 'graph> {
    pub(in crate::call_order) fn new(spec: &'spec CallOrderSpec, inputs: &[KeyedUnit<'src, '_, 'graph>]) -> Self {
        let mut program = Self {
            frames: Vec::new(),
            targets: BTreeMap::new(),
            associated: Associated::default(),
            paths: BTreeMap::new(),
            unresolved: RefCell::new(BTreeSet::new()),
            units: Vec::new(),
        };
        for (unit, input) in inputs.iter().enumerate() {
            program.associated.index_types(unit, input);
            program.paths.insert(input.path, unit);
            program.units.push(Unit {
                path: input.path,
                source: input.source,
                imports: input.imports,
                exports: input.exports,
                entry: input.entry,
                frames: Vec::new(),
            });
            for function in input.functions {
                program.add_frame(spec, unit, input.source, function);
            }
        }
        program
    }

    fn add_frame(
        &mut self,
        spec: &'spec CallOrderSpec,
        unit: usize,
        source: &'src [u8],
        function: &KeyedFunction<'_, 'graph>,
    ) {
        let node = function.node;
        let scope = scope(node, unit);
        let params = parameters(node, source);
        let export = params.as_ref().and_then(|params| {
            let name = candidate(node, source)?;
            self.targets
                .entry((scope, name.clone(), params.len()))
                .and_modify(|target| *target = None)
                .or_insert(Some(self.frames.len()));
            self.associated.register(unit, (node, source), &name, params.len(), self.frames.len());
            Some(export(node, source, name))
        });
        self.units[unit].frames.push(self.frames.len());
        self.frames.push(Frame {
            graph: function.graph,
            plan: Plan::new(spec, function.graph, node, source, unit),
            unit,
            scope,
            unsupported_rust_line: (node.kind() == "function_item" && !crate::lower::rust::supports_function(node))
                .then_some(node.start_position().row as u32 + 1),
            params: params.unwrap_or_default(),
            export,
        });
    }

    pub(in crate::call_order) fn path(&self, unit: usize) -> Option<String> {
        let path = self.units[unit].path;
        (!path.is_empty()).then(|| path.to_owned())
    }

    pub(in crate::call_order) fn has_registration(&self) -> bool {
        self.frames.iter().filter_map(|frame| frame.plan.as_ref()).any(Plan::has_registration)
    }

    pub(in crate::call_order) fn target(&self, from: usize, name: &str, site: &CallSite) -> Option<usize> {
        let frame = &self.frames[from];
        let root = name.split(['.', ':']).next()?;
        let syntax = &frame.plan.as_ref()?.syntax;
        if name.contains("::") && (syntax.shadows_type(root) || syntax.has_local_type_imports()) {
            self.unresolved.borrow_mut().insert((from, site.start, "unresolved_helper_target"));
            return None;
        }
        if syntax.taken_name(root) || syntax.shadows_type(root) {
            return None;
        }
        let arity = match site.arguments.as_slice() {
            [Some(crate::cfg::IdentityOperand::CallResult { start, .. })] => frame.plan.as_ref()?
                .result_components.get(start).map_or(1, |shape| shape.count),
            _ => site.arguments.len(),
        };
        if let Some(target) = self.associated.lookup(frame.unit, frame.export.as_ref(), name, arity) {
            if target.is_none() { self.unresolved.borrow_mut().insert((from, site.start, "unresolved_helper_target")); }
            return target;
        }
        self.resolve_named(from, name, root, site, arity)
    }

    pub(super) fn roots(&self) -> Vec<usize> {
        let entries: Vec<_> = self
            .units
            .iter()
            .filter(|unit| unit.entry)
            .flat_map(|unit| unit.frames.iter().copied())
            .collect();
        let mut pending = entries.clone();
        let mut incoming = BTreeSet::new();
        let mut seen = BTreeSet::new();
        while let Some(from) = pending.pop() {
            if seen.insert(from) {
                let frame = &self.frames[from];
                if let Some(plan) = &frame.plan {
                    let targets = self.resolved_calls(from, frame.graph, plan);
                    let targets: BTreeSet<_> = targets.into_iter()
                        .map(|(_, target)| target).filter(|target| *target != from).collect();
                    pending.extend(targets.iter().copied());
                    incoming.extend(targets);
                }
            }
        }
        entries
            .into_iter()
            .filter(|index| !incoming.contains(index))
            .collect()
    }

    pub(in crate::call_order) fn resolution_limitations(&self) -> Vec<super::KeyedLimitation> {
        if self.unresolved.borrow().is_empty() && self.frames.iter().all(|frame| frame.unsupported_rust_line.is_none()) {
            return Vec::new();
        }
        let mut pending: Vec<_> = self.units.iter().filter(|unit| unit.entry)
            .flat_map(|unit| unit.frames.iter().copied()).collect();
        let mut reachable = BTreeSet::new();
        while let Some(index) = pending.pop() {
            if !reachable.insert(index) { continue; }
            let frame = &self.frames[index];
            if let Some(plan) = &frame.plan {
                pending.extend(self.resolved_calls(index, frame.graph, plan).into_iter().map(|(_, target)| target));
            }
        }
        let mut limitations: Vec<_> = reachable.iter().filter_map(|&frame| self.frames[frame].unsupported_rust_line.map(|line| super::KeyedLimitation {
            reason: "unsupported_rust_control_flow", path: self.path(self.frames[frame].unit), line: Some(line),
        })).collect();
        limitations.extend(self.unresolved.borrow().iter().filter_map(|&(frame, start, reason)| {
            if !reachable.contains(&frame) { return None; }
            let unit = &self.units[self.frames[frame].unit];
            let line = unit.source.get(..start).map(|before| before.iter().filter(|byte| **byte == b'\n').count() as u32 + 1);
            Some(super::KeyedLimitation { reason, path: self.path(self.frames[frame].unit), line })
        }));
        limitations
    }

    /// Return reachable modeled calls with their source positions and unique
    /// target frames. The frame identifies the source file for each position.
    pub(in crate::call_order) fn resolved_calls(
        &self,
        from: usize,
        cfg: &Cfg<Stmt>,
        plan: &Plan<'_, '_>,
    ) -> BTreeSet<(usize, usize)> {
        let mut resolved = BTreeSet::new();
        let nodes = reachable_nodes(cfg);
        for call in nodes.iter().filter_map(|node| node.throwing_call.as_ref()) {
            if let Some(site) = &call.site {
                self.record_target(from, &call.name, site, plan, &mut resolved);
            }
        }
        for stmt in nodes.iter().flat_map(|node| &node.stmts) {
            let (name, site) = match stmt {
                Stmt::Call {
                    target: CallTarget::Named(name),
                    site: Some(site),
                    ..
                }
                | Stmt::DeferredCall {
                    target: name,
                    site: Some(site),
                } => (name, site),
                Stmt::DeferredBlock(body) => {
                    resolved.extend(self.resolved_calls(from, body, plan));
                    continue;
                }
                _ => continue,
            };
            self.record_target(from, name, site, plan, &mut resolved);
        }
        resolved
    }

    fn record_target(
        &self,
        from: usize,
        name: &str,
        site: &CallSite,
        plan: &Plan<'_, '_>,
        resolved: &mut BTreeSet<(usize, usize)>,
    ) {
        // A verified result source is modeled at its call site, not as an
        // unresolved helper body. Its exception paths remain in the CFG.
        if plan.extra_sources.contains(&site.start) { return; }
        if plan.may_throw(name, site.start, &Values::new()) {
            if let Some(callee) = self.target(from, name, site) {
                resolved.insert((site.start, callee));
            }
        }
    }
}

impl Frame<'_, '_, '_> {
    fn matches_export(&self, binding: &KeyedImport, name: &str, arity: usize) -> bool {
        self.params.len() == arity
            && self.export.as_ref().is_some_and(|export| {
                (export.name == name || (name == "default" && export.default))
                    && export.owner == binding.owner
                    && (!binding.exported || export.visible)
            })
    }

    pub(super) fn bind(&self, arguments: Vec<Option<IdentityOperand>>) -> Values {
        self.params.iter().cloned().zip(arguments).collect()
    }
}

impl KeyedImport {
    fn target<'a>(&'a self, called: &'a str) -> Option<&'a str> {
        match &self.name {
            Some(name) => (self.local == called).then_some(name),
            None => {
                let member = called.strip_prefix(&self.local)?;
                (!member.is_empty() && !member.contains(['.', ':'])).then_some(member)
            }
        }
    }
}

fn reachable_nodes(cfg: &Cfg<Stmt>) -> Vec<&crate::cfg::CfgNode<Stmt>> {
    let edges = super::outgoing_edges(cfg);
    let mut seen = BTreeSet::new();
    let mut work = vec![cfg.entry];
    while let Some(id) = work.pop() {
        if seen.insert(id) {
            work.extend(edges[id].iter().map(|(to, _)| *to));
        }
    }
    seen.into_iter().map(|id| &cfg.nodes[id]).collect()
}

fn scope(node: Node<'_>, unit: usize) -> Scope {
    std::iter::successors(node.parent(), |parent| parent.parent())
        .find(|parent| matches!(parent.kind(), "class_body" | "interface_body" | "enum_body" | "impl_item"))
        .map_or(Scope::File(unit), |parent| {
            Scope::Class(unit, parent.start_byte())
        })
}
