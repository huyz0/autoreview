use std::collections::HashSet;
use tree_sitter::Node;

use crate::cfg::{CallSite, CallTarget, Cfg, Stmt};

use super::identity::{Entry, Syntax};
use super::CallOrderSpec;

#[derive(Default)]
pub(super) struct Step {
    pub effect: Option<Effect>,
    pub trigger: Option<(Entry, String)>,
    pub exit: Option<&'static str>,
}

pub(super) enum Effect {
    Register(Entry),
    Remove(Entry),
    Defer(Entry),
}

pub(super) fn prepare(
    spec: &CallOrderSpec,
    cfg: &Cfg<Stmt>,
    function: Node<'_>,
    source: &[u8],
) -> Option<Vec<Vec<Step>>> {
    if cfg.keyed_flow == Some(0) || !has_calls(cfg) {
        return None;
    }
    let mut planner = Planner {
        spec,
        syntax: Syntax::scan(function, source)?,
        modeled: HashSet::new(),
    };
    let steps = cfg
        .nodes
        .iter()
        .map(|node| {
            node.stmts
                .iter()
                .map(|stmt| planner.step(stmt))
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<Vec<_>>>()?;
    planner
        .syntax
        .models_cleanup(&spec.unless, &planner.modeled)
        .then_some(steps)
}

fn has_calls(cfg: &Cfg<Stmt>) -> bool {
    cfg.nodes.iter().flat_map(|node| &node.stmts).any(|stmt| {
        matches!(
            stmt,
            Stmt::Call {
                target: CallTarget::Named(_),
                site: Some(_),
                ..
            }
        )
    })
}

struct Planner<'spec, 'src> {
    spec: &'spec CallOrderSpec,
    syntax: Syntax<'src>,
    modeled: HashSet<usize>,
}

impl Planner<'_, '_> {
    fn step(&mut self, stmt: &Stmt) -> Option<Step> {
        match stmt {
            Stmt::Call {
                target: CallTarget::Named(name),
                site,
                ..
            } => self.call(name, site.as_deref(), false),
            Stmt::DeferredCall { target, site } => self.call(target, site.as_deref(), true),
            Stmt::Return { .. } => Some(Step {
                exit: Some("return"),
                ..Step::default()
            }),
            Stmt::Throw => Some(Step {
                exit: Some("throw"),
                ..Step::default()
            }),
            _ => Some(Step::default()),
        }
    }

    fn call(&mut self, name: &str, site: Option<&CallSite>, deferred: bool) -> Option<Step> {
        let start = site.map_or(0, |site| site.start);
        let matches = |patterns| self.syntax.matches(patterns, name, start);
        let register = matches(&self.spec.after);
        let remove = matches(&self.spec.unless);
        let trigger = matches(&self.spec.before);
        if deferred && (register || trigger) {
            return None;
        }

        let entry = site.and_then(|site| self.syntax.entry(self.spec.identity?, site));
        let trigger = entry
            .as_ref()
            .filter(|_| trigger)
            .map(|entry| (entry.clone(), name.to_owned()));
        let effect = if remove {
            self.modeled.insert(site?.start);
            let pair = entry?;
            Some(if deferred {
                Effect::Defer(pair)
            } else {
                Effect::Remove(pair)
            })
        } else {
            entry.filter(|_| register).map(Effect::Register)
        };
        Some(Step {
            effect,
            trigger,
            exit: None,
        })
    }
}
