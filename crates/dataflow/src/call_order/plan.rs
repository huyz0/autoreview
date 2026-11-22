use std::collections::HashSet;
use tree_sitter::Node;

use crate::cfg::{CallSite, Cfg, Stmt};

use super::identity::{Entry, Syntax, Values};
use super::{CallOrderSpec, EntryIdentity};

pub(super) struct Plan<'a, 'src> {
    pub spec: &'a CallOrderSpec,
    pub syntax: Syntax<'src>,
    selector: EntryIdentity,
}

pub(super) struct Call {
    pub registered: bool,
    pub removed: bool,
    pub trigger: bool,
    pub entry: Option<Entry>,
}

impl<'a, 'src> Plan<'a, 'src> {
    pub(super) fn new(
        spec: &'a CallOrderSpec,
        cfg: &Cfg<Stmt>,
        function: Node<'_>,
        source: &'src [u8],
    ) -> Option<Self> {
        let mut sources = Sources::default();
        sources.collect(cfg)?;
        let syntax = Syntax::scan(function, source, &sources.writes)?;
        syntax
            .models_cleanup(&spec.unless, &sources.calls)
            .then_some(Self {
                spec,
                syntax,
                selector: spec.identity?,
            })
    }

    pub(super) fn call(
        &self,
        name: &str,
        site: Option<&CallSite>,
        values: &Values,
    ) -> Option<Call> {
        let start = site.map_or(0, |site| site.start);
        let matches = |patterns| self.syntax.matches(patterns, name, start, values);
        let removed = matches(&self.spec.unless);
        let entry = site.and_then(|site| self.syntax.entry(self.selector, site, values));
        if entry.is_none()
            && self
                .syntax
                .may_match_cleanup(&self.spec.unless, name, start, values)
        {
            return None;
        }
        Some(Call {
            registered: !removed && matches(&self.spec.after),
            removed,
            trigger: matches(&self.spec.before),
            entry,
        })
    }

    pub(super) fn may_throw(&self, name: &str, start: usize, values: &Values) -> bool {
        // A call selected by this rule is a modeled event. Opaque calls can
        // transfer before completing their ordinary statement.
        [&self.spec.after, &self.spec.unless, &self.spec.before]
            .into_iter()
            .all(|patterns| !self.syntax.matches(patterns, name, start, values))
    }
}

#[derive(Default)]
struct Sources {
    calls: HashSet<usize>,
    writes: HashSet<usize>,
}

impl Sources {
    fn collect(&mut self, cfg: &Cfg<Stmt>) -> Option<()> {
        if cfg.keyed_flow == Some(0) {
            return None;
        }
        for stmt in cfg.nodes.iter().flat_map(|node| &node.stmts) {
            match stmt {
                Stmt::Call {
                    site: Some(site), ..
                }
                | Stmt::DeferredCall {
                    site: Some(site), ..
                } => {
                    self.calls.insert(site.start);
                }
                Stmt::Rebind(write) => {
                    self.writes.insert(write.start);
                }
                Stmt::DeferredBlock(body) => self.collect(body)?,
                _ => {}
            }
        }
        Some(())
    }
}
