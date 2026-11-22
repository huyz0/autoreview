use std::collections::BTreeMap;

use autoreview_dataflow::{solver, Cfg, Lattice, Stmt};

use super::calls::Endpoints;
use super::Node;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Seen {
    reachable: bool,
    write: Option<usize>,
    publish: Option<usize>,
}

impl Lattice for Seen {
    fn bottom() -> Self {
        Self::default()
    }

    fn join(&self, other: &Self) -> Self {
        Self {
            reachable: self.reachable || other.reachable,
            write: first(self.write, other.write),
            publish: first(self.publish, other.publish),
        }
    }
}

fn first(left: Option<usize>, right: Option<usize>) -> Option<usize> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        _ => left.or(right),
    }
}

#[derive(Clone, Copy)]
enum Call {
    Write(usize),
    Publish(usize),
}

impl Call {
    fn from_statement(statement: &Stmt, endpoints: &Endpoints<'_>) -> Option<Self> {
        let Stmt::Call {
            site: Some(site), ..
        } = statement
        else {
            return None;
        };
        if endpoints.write.contains_key(&site.start) {
            Some(Self::Write(site.start))
        } else if endpoints.publish.contains_key(&site.start) {
            Some(Self::Publish(site.start))
        } else {
            None
        }
    }

    fn advance(self, seen: &mut Seen) -> Option<(usize, usize)> {
        match self {
            Self::Write(write) => {
                seen.write = first(seen.write, Some(write));
                seen.publish.map(|publish| (publish, write))
            }
            Self::Publish(publish) => {
                seen.publish = first(seen.publish, Some(publish));
                seen.write.map(|write| (publish, write))
            }
        }
    }
}

pub(super) fn pairs<'tree>(
    cfg: &Cfg<Stmt>,
    endpoints: &Endpoints<'tree>,
) -> BTreeMap<usize, (Node<'tree>, Node<'tree>)> {
    let calls: Vec<Vec<_>> = cfg
        .nodes
        .iter()
        .map(|node| {
            node.stmts
                .iter()
                .filter_map(|statement| Call::from_statement(statement, endpoints))
                .collect()
        })
        .collect();
    let out = solver::solve(cfg, |index, _, input: &Seen| {
        let mut seen = *input;
        seen.reachable |= index == cfg.entry;
        if seen.reachable {
            for &call in &calls[index] {
                call.advance(&mut seen);
            }
        }
        seen
    });
    let mut findings = BTreeMap::new();
    for (index, calls) in calls
        .iter()
        .enumerate()
        .filter(|(_, calls)| !calls.is_empty())
    {
        let mut seen = cfg.predecessors(index).iter().fold(
            Seen {
                reachable: index == cfg.entry,
                ..Default::default()
            },
            |seen, &node| seen.join(&out[node]),
        );
        if !seen.reachable {
            continue;
        }
        for &call in calls {
            if let Some((publish, write)) = call.advance(&mut seen) {
                findings
                    .entry(publish)
                    .or_insert((endpoints.publish[&publish], endpoints.write[&write]));
            }
        }
    }
    findings
}
