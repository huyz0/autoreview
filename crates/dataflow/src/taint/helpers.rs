use crate::cfg::{CallSite, CallTarget, Cfg, IdentityOperand, Stmt};

use super::{is_tainted, visit_calls, Inputs, TaintHit, TaintSpec};

pub(crate) struct TaintedCall<'a> {
    pub(crate) name: &'a str,
    pub(crate) site: &'a CallSite,
    pub(crate) line: u32,
    pub(crate) arguments: Vec<(usize, &'a str)>,
}

#[derive(Default)]
pub(crate) struct HelperScan<'a> {
    pub(crate) sinks: Vec<TaintHit>,
    pub(crate) calls: Vec<TaintedCall<'a>>,
}

pub(crate) fn has_direct_source(spec: &TaintSpec, cfg: &Cfg<Stmt>) -> bool {
    cfg.nodes.iter().flat_map(|node| &node.stmts).any(|stmt| {
        matches!(stmt, Stmt::Call { target: CallTarget::Named(name), assigned_to: Some(_), .. }
            if spec.sources.iter().any(|source| source.matches(name)))
    })
}

pub(crate) fn scan_helper_calls<'a>(spec: &TaintSpec, cfg: &'a Cfg<Stmt>, seed: Option<&str>) -> HelperScan<'a> {
    let mut scan = HelperScan::default();
    visit_calls(spec, cfg, Inputs::Helper(seed), |call, facts| {
        let Some(site) = call.site else { return };
        // Unmodeled operands can include Kotlin named arguments. Do not guess their mapping.
        if !site.arguments.iter().all(Option::is_some) {
            return;
        }
        let arguments: Vec<_> = site.arguments.iter().enumerate().filter_map(|(index, operand)| {
            let IdentityOperand::Binding(name) = operand.as_ref()? else { return None };
            is_tainted(facts, name).then_some((index, name.as_str()))
        }).collect();
        if seed.is_some() {
            for &(index, arg) in &arguments {
                if spec.sinks.iter().any(|sink| sink.call.matches(call.name)
                    && sink.tainted_arg_positions.as_ref().is_none_or(|positions| positions.contains(&index))) {
                    scan.sinks.push(call.hit(arg));
                }
            }
        }
        if !arguments.is_empty() {
            scan.calls.push(TaintedCall { name: call.name, site, line: call.line, arguments });
        }
    });
    scan
}
