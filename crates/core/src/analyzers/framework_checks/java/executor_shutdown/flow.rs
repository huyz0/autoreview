use std::collections::BTreeMap;

use autoreview_dataflow::{lower, solver, Cfg, Lattice, Stmt};
use autoreview_langsupport::Language;
use tree_sitter::Node;

use super::super::Java;
use super::ExecutorCall;

pub(super) fn unfinished(
    java: &Java<'_>,
    callback: Node<'_>,
    calls: &[ExecutorCall<'_>],
) -> Vec<usize> {
    let cfg = lower::keyed_variant(Language::Java, java.source.as_bytes(), callback);
    let calls_by_site: BTreeMap<_, _> = calls
        .iter()
        .map(|call| (call.node.start_byte(), call))
        .collect();
    let events = events(java, &cfg, &calls_by_site);
    calls
        .iter()
        .filter(|call| call.method == "shutdown")
        .filter(|call| returns_pending(&cfg, &events, call))
        .map(|call| call.node.start_byte())
        .collect()
}

enum Event<'a, 't> {
    Call(&'a ExecutorCall<'t>),
    Throw,
}

fn events<'a, 't>(
    java: &Java<'_>,
    cfg: &Cfg<Stmt>,
    by_site: &BTreeMap<usize, &'a ExecutorCall<'t>>,
) -> Vec<Vec<Event<'a, 't>>> {
    cfg.nodes
        .iter()
        .map(|node| {
            let mut events: Vec<_> = node
                .stmts
                .iter()
                .filter_map(|stmt| match stmt {
                    Stmt::Call {
                        site: Some(site), ..
                    } => by_site.get(&site.start).map(|call| Event::Call(call)),
                    Stmt::Throw { .. } => Some(Event::Throw),
                    _ => None,
                })
                .collect();
            if let Some(call) = node.condition_span().and_then(|span| {
                let condition = java.root.descendant_for_byte_range(span.start, span.end)?;
                by_site.get(&guard_call(condition)?.start_byte()).copied()
            }) {
                events.push(Event::Call(call));
            }
            events
        })
        .collect()
}

fn guard_call(mut condition: Node<'_>) -> Option<Node<'_>> {
    while matches!(
        condition.kind(),
        "parenthesized_expression" | "unary_expression"
    ) {
        condition = condition.named_child(0)?;
    }
    (condition.kind() == "method_invocation").then_some(condition)
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct States(u8);

impl Lattice for States {
    fn bottom() -> Self {
        Self(0)
    }

    fn join(&self, other: &Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl States {
    const LIVE: u8 = 1;
    const PENDING: u8 = 2;
    const COMPLETED: u8 = 4;

    fn apply(self, event: &Event<'_, '_>, target: &ExecutorCall<'_>) -> Self {
        if self.0 == 0 {
            return self;
        }
        let Event::Call(call) = event else {
            return Self(0);
        };
        if call.identity != target.identity {
            return self;
        }
        match call.method {
            "shutdown" if call.node == target.node => self.advance(Self::LIVE, Self::PENDING),
            "awaitTermination" => self.advance(Self::PENDING, Self::COMPLETED),
            "close" | "shutdownNow" => Self(Self::COMPLETED),
            _ => self,
        }
    }

    fn advance(self, from: u8, to: u8) -> Self {
        if self.0 & from == 0 {
            return self;
        }
        Self(self.0 & !from | to)
    }
}

fn returns_pending(
    cfg: &Cfg<Stmt>,
    events: &[Vec<Event<'_, '_>>],
    target: &ExecutorCall<'_>,
) -> bool {
    let states = solver::solve(cfg, |index, _, input: &States| {
        let initial = if index == cfg.entry {
            input.join(&States(States::LIVE))
        } else {
            *input
        };
        events[index]
            .iter()
            .fold(initial, |states, event| states.apply(event, target))
    });
    states[cfg.exit].0 & States::PENDING != 0
}
