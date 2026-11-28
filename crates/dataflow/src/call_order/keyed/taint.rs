use std::collections::BTreeMap;

use tree_sitter::Node;

use crate::cfg::CallSite;
use crate::lower::site::children;
use crate::taint::{has_direct_source, scan_helper_calls, TaintHit, TaintSpec, TaintedCall};

use super::program::{DefaultArgument, KotlinTarget};
use super::{KeyedUnit, Program};

const HELPER_HOPS: usize = 2;

pub struct HelperTaintHit {
    pub caller_path: String,
    pub caller_line: u32,
    pub caller_argument: String,
    pub sink_path: String,
    pub sink: TaintHit,
}

/// Trace values from sources in entry files into directly resolved JVM helpers.
pub fn check_jvm_taint_helpers(spec: &TaintSpec, units: &[KeyedUnit<'_, '_, '_>]) -> Vec<HelperTaintHit> {
    if !units.iter().filter(|unit| unit.entry).flat_map(|unit| unit.functions)
        .any(|function| has_direct_source(spec, function.graph)) {
        return Vec::new();
    }
    let program = Program::for_resolution(units);
    let eligible = units.iter().flat_map(|unit| unit.functions.iter().map(|fun| {
        fun.graph.keyed_flow != Some(0) && eligible_helper(fun.node)
    })).collect();
    // A summary describes the seeded parameter's effect. Independent sources
    // inside the helper are already reported by the ordinary taint check.
    let mut parameter_spec = spec.clone();
    parameter_spec.sources.clear();
    let mut analysis = Analysis { program: &program, eligible, spec: parameter_spec, summaries: BTreeMap::new() };
    analysis.hits(spec)
}

#[derive(Clone)]
struct Sink {
    unit: usize,
    hit: TaintHit,
}

impl Sink {
    fn key(&self) -> (usize, u32, &str) {
        (self.unit, self.hit.source_line, &self.hit.sink_call)
    }
}

type HitKey = (usize, u32, usize, u32, String);

struct Analysis<'a, 'src, 'graph> {
    program: &'a Program<'a, 'src, 'graph>,
    eligible: Vec<bool>,
    spec: TaintSpec,
    summaries: BTreeMap<(usize, usize, usize), Vec<Sink>>,
}

impl Analysis<'_, '_, '_> {
    fn hits(&mut self, spec: &TaintSpec) -> Vec<HelperTaintHit> {
        let mut hits = BTreeMap::new();
        for root in self.program.entry_frames() {
            let frame = &self.program.frames[root];
            if !has_direct_source(spec, frame.graph) {
                continue;
            }
            for call in scan_helper_calls(spec, frame.graph, None).calls {
                self.record_call(root, call, &mut hits);
            }
        }
        hits.into_values().collect()
    }

    fn record_call(&mut self, root: usize, mut call: TaintedCall<'_>, hits: &mut BTreeMap<HitKey, HelperTaintHit>) {
        let Some(target) = self.bind_call(root, &mut call) else { return };
        let unit = self.program.frames[root].unit;
        let Some(caller_path) = self.program.path(unit) else { return };
        for (slot, arg) in call.arguments {
            for sink in self.summarize(target, slot, HELPER_HOPS) {
                let Some(sink_path) = self.program.path(sink.unit) else { continue };
                let key = (unit, call.line, sink.unit, sink.hit.source_line, sink.hit.sink_call.clone());
                hits.entry(key).or_insert_with(|| HelperTaintHit {
                    caller_path: caller_path.clone(), caller_line: call.line, caller_argument: arg.to_owned(),
                    sink_path, sink: sink.hit,
                });
            }
        }
    }

    fn bind_call(&self, from: usize, call: &mut TaintedCall<'_>) -> Option<usize> {
        let target = match self.program.kotlin_target(from, call.name, call.site) {
            KotlinTarget::Declined => return None,
            KotlinTarget::Positional => self.program.target(from, call.name, call.site)?,
            KotlinTarget::Bound(target) => {
                let frame = &self.program.frames[target];
                bind_kotlin_arguments(call, &frame.params, &frame.kotlin_defaults)?;
                target
            }
        };
        self.eligible[target].then_some(target)
    }

    fn summarize(&mut self, target: usize, slot: usize, hops: usize) -> Vec<Sink> {
        let key = (target, slot, hops);
        if let Some(sinks) = self.summaries.get(&key) {
            return sinks.clone();
        }
        let frame = &self.program.frames[target];
        let Some(parameter) = frame.params.get(slot) else { return Vec::new() };
        let scan = scan_helper_calls(&self.spec, frame.graph, Some(parameter));
        let mut sinks: Vec<_> = scan.sinks.into_iter().map(|hit| Sink { unit: frame.unit, hit }).collect();
        if hops > 1 {
            for mut call in scan.calls {
                let Some(callee) = self.bind_call(target, &mut call) else { continue };
                for (slot, _) in call.arguments {
                    sinks.extend(self.summarize(callee, slot, hops - 1));
                }
            }
        }
        sinks.sort_by(|a, b| a.key().cmp(&b.key()));
        sinks.dedup_by(|a, b| a.key() == b.key());
        self.summaries.insert(key, sinks.clone());
        sinks
    }
}

fn bind_kotlin_arguments(call: &mut TaintedCall<'_>, parameters: &[String], defaults: &[DefaultArgument]) -> Option<()> {
    let sources = kotlin_argument_sources(call.site, parameters, defaults)?;
    let mut tainted = vec![None; call.site.arguments.len()];
    for (index, argument) in call.arguments.drain(..) {
        tainted[index] = Some(argument);
    }
    call.arguments.extend(sources.into_iter().enumerate().filter_map(|(slot, source)| {
        Some((slot, tainted[source?]?))
    }));
    Some(())
}

fn kotlin_argument_sources(site: &CallSite, parameters: &[String], defaults: &[DefaultArgument]) -> Option<Vec<Option<usize>>> {
    if site.arguments.len() > parameters.len() || defaults.len() != parameters.len() {
        return None;
    }
    let mut sources = vec![None; parameters.len()];
    let mut named = site.named_arguments.iter().peekable();
    let mut reordered = false;
    for index in 0..site.arguments.len() {
        let slot = match named.next_if(|arg| arg.index == index) {
            Some(arg) => parameters.iter().position(|name| *name == arg.label)?,
            None if !reordered => index,
            None => return None,
        };
        if sources[slot].replace(index).is_some() {
            return None;
        }
        reordered |= slot != index;
    }
    for (slot, default) in defaults.iter().enumerate() {
        if sources[slot].is_none() {
            sources[slot] = match default {
                DefaultArgument::Literal => None,
                DefaultArgument::Alias(earlier) => sources[*earlier],
                DefaultArgument::Required | DefaultArgument::Unsupported => return None,
            };
        }
    }
    Some(sources)
}

fn eligible_helper(node: Node<'_>) -> bool {
    match node.kind() {
        "function_declaration" => node.parent().is_some_and(|parent| parent.kind() == "source_file"),
        "method_declaration" => {
            let Some(class) = node.parent().and_then(|body| body.parent()) else { return false };
            class.kind() == "class_declaration" && class.child_by_field_name("superclass").is_none()
                && (has_modifier(node, &["static", "private", "final"]) || has_modifier(class, &["final"]))
        }
        _ => false,
    }
}

fn has_modifier(node: Node<'_>, names: &[&str]) -> bool {
    children(node).find(|child| child.kind() == "modifiers").is_some_and(|modifiers| {
        modifiers.children(&mut modifiers.walk()).any(|modifier| names.contains(&modifier.kind()))
    })
}
