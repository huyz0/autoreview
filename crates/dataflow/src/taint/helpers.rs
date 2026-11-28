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
        let mut named = site.named_arguments.iter().peekable();
        let mut arguments = Vec::new();
        for (index, positional) in site.arguments.iter().enumerate() {
            let operand = named.next_if(|arg| arg.index == index).map_or(positional, |arg| &arg.value);
            let Some(operand) = operand else { return };
            if let IdentityOperand::Binding(name) = operand {
                if is_tainted(facts, name) { arguments.push((index, name.as_str())); }
            }
        }
        // Sink rules use declaration positions and do not describe parameter names.
        if seed.is_some() && site.named_arguments.is_empty() {
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
