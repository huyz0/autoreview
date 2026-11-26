use std::collections::{BTreeMap, BTreeSet, HashSet};
use tree_sitter::Node;

use crate::cfg::{CallSite, Cfg, Stmt};

use super::identity::{Entry, Syntax, Values};
use super::contract::ApiMatcher;
use super::{CallOrderSpec, ContractMode};

pub(super) struct Plan<'a, 'src> {
    pub spec: &'a CallOrderSpec,
    pub syntax: Syntax<'src>,
    api: Option<ApiMatcher<'a>>,
    pub(super) extra_sources: BTreeSet<usize>,
    pub(super) result_components: BTreeMap<usize, super::ResultComponents>,
}

pub(super) struct Call {
    pub registered: bool,
    pub removed: bool,
    pub trigger: bool,
    pub entry: Option<Entry>,
}

impl<'a, 'src> Plan<'a, 'src> {
    pub(super) fn declined_cleanup(
        spec: &CallOrderSpec, cfg: &Cfg<Stmt>, function: Node<'_>, source: &[u8], unit: usize,
    ) -> Option<(u32, bool)> {
        if spec.contract.is_some() || spec.unless.is_empty() { return None; }
        let mut sources = Sources::default();
        sources.collect(cfg)?;
        let syntax = Syntax::scan(function, source, &sources.writes, unit, false)?;
        let start = syntax.unmodeled_cleanup(&spec.unless, &sources.calls)?;
        let line = source.get(..start)?.iter().filter(|byte| **byte == b'\n').count() as u32 + 1;
        Some((line, syntax.has_registration(&spec.after)))
    }

    pub(super) fn new(
        spec: &'a CallOrderSpec,
        cfg: &Cfg<Stmt>,
        function: Node<'_>,
        source: &'src [u8],
        unit: usize,
    ) -> Option<Self> {
        let mut sources = Sources::default();
        sources.collect(cfg)?;
        let syntax = Syntax::scan(function, source, &sources.writes, unit, spec.tracks_result_use())?;
        let api = spec.contract.as_ref().map(|contract| ApiMatcher::new(contract, function, source));
        if api.as_ref().is_some_and(|api| !api.supports_local_handles(function, source)) {
            return None;
        }
        syntax
            .models_cleanup(&spec.unless, &sources.calls)
            .then_some(Self {
                spec,
                syntax,
                api,
                extra_sources: BTreeSet::new(),
                result_components: BTreeMap::new(),
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
        let removed = matches(&self.spec.unless) && (
            !self.spec.contract.as_ref().is_some_and(|contract| contract.complete_observed)
            || site.is_some_and(|site| site.observed)
        );
        let entry = site.and_then(|site| {
            if let Some(selector) = self.spec.identity {
                return self.syntax.entry(selector, site, values);
            }
            let receiver = site.receiver.as_deref()?;
            self.syntax.resolve(values, receiver, site.start).map(Entry::handle)
        });
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

    pub(super) fn has_registration(&self) -> bool {
        if !self.extra_sources.is_empty() { return true; }
        match &self.api {
            Some(api) => self.syntax.any_call(|name| api.matches(name, |root| self.syntax.shadows_api(root))),
            None => self.syntax.has_registration(&self.spec.after),
        }
    }

    pub(super) fn source_mode(&self, name: &str) -> Option<ContractMode> {
        let api = self.api.as_ref()?;
        api.matches(name, |root| self.syntax.shadows_api(root))
            .then_some(api.mode())
    }

    pub(super) fn source_mode_at(&self, name: &str, start: usize) -> Option<ContractMode> {
        self.extra_sources.contains(&start).then_some(ContractMode::MustUse).or_else(|| self.source_mode(name))
    }

    pub(super) fn is_completion(&self, name: &str, start: usize, values: &Values) -> bool {
        self.syntax.matches(&self.spec.unless, name, start, values)
    }

    pub(super) fn needs_completion(&self, site: &CallSite) -> bool {
        let requires_await = self.spec.contract.as_ref().is_some_and(|contract| contract.acquire_awaited);
        (!requires_await || site.awaited) && !(site.awaited && self.spec.unless.iter().any(|pattern| pattern.matches("await")))
    }

    pub(super) fn may_throw(&self, name: &str, start: usize, values: &Values) -> bool {
        // A call selected by this rule is a modeled event. Opaque calls can
        // transfer before completing their ordinary statement.
        (self.source_mode(name).is_none()
            || self.spec.contract.as_ref().is_some_and(|contract| contract.acquire_awaited))
            && [&self.spec.after, &self.spec.unless, &self.spec.before]
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
