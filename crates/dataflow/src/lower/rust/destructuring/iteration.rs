use super::{branch_value, iterator_bindings, local, pattern_pairs, Projection, Bindings, Lower, Node, NodeId};
use crate::cfg::EdgeKind;
use crate::lower::{line, site::children};

const MAX_PROJECTED_ITEMS: usize = 8;

pub(in crate::lower::rust) struct Iteration<'tree> {
    pub(super) items: Vec<Projection<'tree>>,
    local: bool,
}

pub(in crate::lower::rust) fn plan<'tree>(loop_expr: Node<'tree>, source: &[u8]) -> Option<Iteration<'tree>> {
    if iterator_bindings(loop_expr, source)?.is_empty() {
        return None;
    }
    let pattern = loop_expr.child_by_field_name("pattern")?;
    let iterable = branch_value::ungroup_value(loop_expr.child_by_field_name("value")?);
    let local = local::source(loop_expr, source);
    let array = match local {
        Some((_, value)) => value,
        None => iterable,
    };
    if array.kind() != "array_expression" || array.child_by_field_name("length").is_some()
        || contains_iterator(loop_expr.child_by_field_name("body")?)
    {
        return None;
    }
    let mut items = Vec::new();
    for value in children(array).take(MAX_PROJECTED_ITEMS + 1) {
        if items.len() == MAX_PROJECTED_ITEMS { return None; }
        items.push(pattern_pairs(pattern, value, source, true)?);
    }
    Some(Iteration { items, local: local.is_some() })
}

fn contains_iterator(body: Node<'_>) -> bool {
    let mut pending = vec![body];
    while let Some(node) = pending.pop() {
        match node.kind() {
            "for_expression" => return true,
            "closure_expression" | "function_item" | "async_block" => {}
            _ => pending.extend(children(node)),
        }
    }
    false
}

impl Lower<'_> {
    pub(in crate::lower::rust) fn projected_iteration(
        &mut self, expr: Node<'_>, at: NodeId, names: &Bindings, plan: Iteration<'_>,
    ) -> Option<NodeId> {
        let body = expr.child_by_field_name("body")?;
        let origins = plan.items.iter().flatten().map(|(_, origin)| *origin);
        let at = if plan.local { at } else { self.stage_pattern_values(origins, at, names)? };
        let after = self.cfg.push_node(expr.end_position().row as u32 + 1);
        let entries: Vec<_> = plan.items.iter().map(|_| self.cfg.push_node(line(body)))
            .chain(std::iter::once(after)).collect();
        self.edge(at, entries[0], EdgeKind::Fallthrough);
        if plan.items.is_empty() { self.skip_calls([body]); }

        for (pairs, step) in plan.items.into_iter().zip(entries.windows(2)) {
            self.lower_iteration_item(expr, body, [step[0], step[1], after], names, pairs);
        }
        self.has_incoming(after).then_some(after)
    }

    fn lower_iteration_item(
        &mut self, expr: Node<'_>, body: Node<'_>, [entry, next, after]: [NodeId; 3],
        names: &Bindings, pairs: Projection<'_>,
    ) {
        self.push_loop(expr, next, after);
        let mut scope = names.clone();
        self.bind_staged_pattern(pairs, entry, &mut scope);
        if let Some(end) = self.block(body, entry, &scope) {
            self.edge(end, next, EdgeKind::Fallthrough);
        }
        self.loops.pop();
    }
}
