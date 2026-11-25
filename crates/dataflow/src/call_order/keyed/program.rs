use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

use crate::cfg::{CallSite, CallTarget, Cfg, IdentityOperand, Stmt};
mod signature;
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
    pub entry: bool,
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
    units: Vec<Unit<'src, 'graph>>,
    paths: BTreeMap<&'src str, usize>,
}

pub(in crate::call_order) struct Frame<'spec, 'src, 'graph> {
    pub graph: &'graph Cfg<Stmt>,
    pub unit: usize,
    export: Option<Export>,
    pub plan: Option<Plan<'spec, 'src>>,
    scope: Scope,
    pub(in crate::call_order) params: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
    File(usize),
    Class(usize, usize),
}

struct Unit<'src, 'graph> {
    path: &'src str,
    imports: &'graph [KeyedImport],
    entry: bool,
    frames: Vec<usize>,
}

impl<'spec, 'src, 'graph> Program<'spec, 'src, 'graph> {
    pub(in crate::call_order) fn new(spec: &'spec CallOrderSpec, inputs: &[KeyedUnit<'src, '_, 'graph>]) -> Self {
        let mut program = Self {
            frames: Vec::new(),
            targets: BTreeMap::new(),
            paths: BTreeMap::new(),
            units: Vec::new(),
        };
        for (unit, input) in inputs.iter().enumerate() {
            program.paths.insert(input.path, unit);
            program.units.push(Unit {
                path: input.path,
                imports: input.imports,
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
            Some(export(node, source, name))
        });
        self.units[unit].frames.push(self.frames.len());
        self.frames.push(Frame {
            graph: function.graph,
            plan: Plan::new(spec, function.graph, node, source, unit),
            unit,
            scope,
            params: params.unwrap_or_default(),
            export,
        });
    }

    pub(in crate::call_order) fn path(&self, unit: usize) -> Option<String> {
        let path = self.units[unit].path;
        (!path.is_empty()).then(|| path.to_owned())
    }

    pub(in crate::call_order) fn target(&self, from: usize, name: &str, site: &CallSite) -> Option<usize> {
        let frame = &self.frames[from];
        let root = name.split(['.', ':']).next()?;
        if frame.plan.as_ref()?.syntax.taken_name(root) {
            return None;
        }
        let arity = site.arguments.len();
        if site.local {
            if let Some(local) = self
                .targets
                .get(&(frame.scope, name.into(), arity))
                .or_else(|| {
                    self.targets
                        .get(&(Scope::File(frame.unit), name.into(), arity))
                })
            {
                return *local;
            }
        } else if root == name {
            return None;
        }
        self.imported_target(frame.unit, name, arity)
    }

    fn imported_target(&self, unit: usize, name: &str, arity: usize) -> Option<usize> {
        let imports = self.units[unit].imports;
        let mut matches = BTreeSet::new();
        for binding in imports {
            let Some(target) = binding.target(name) else {
                continue;
            };
            let Some(&provider) = self.paths.get(binding.path.as_str()) else {
                continue;
            };
            matches.extend(
                self.units[provider]
                    .frames
                    .iter()
                    .copied()
                    .filter(|index| self.frames[*index].matches_export(binding, target, arity)),
            );
        }
        (matches.len() == 1).then(|| matches.pop_first()).flatten()
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
        .find(|parent| matches!(parent.kind(), "class_body" | "interface_body" | "enum_body"))
        .map_or(Scope::File(unit), |parent| {
            Scope::Class(unit, parent.start_byte())
        })
}
