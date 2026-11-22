use std::collections::HashSet;

use tree_sitter::Node;

use super::{callback_body, children, descendants, first_argument, text};

pub(super) fn findings<'t>(body: Node<'t>, source: &str) -> Vec<(Node<'t>, Node<'t>)> {
    let statements: Vec<_> = children(body).collect();
    if body.kind() != "statement_block" {
        return Vec::new();
    }
    statements.iter().enumerate().filter_map(|(index, &statement)| {
        let (name, call) = initialized_const(statement, source)?;
        if !called(call, source, "setInterval") {
            return None;
        }
        let mut check = Check { source, missing: None };
        let state = State { aliases: HashSet::from([name]), active: true };
        let end = check.statements(statements[index + 1..].iter().copied(), state, Phase::Setup)?;
        if end.is_some_and(|state| state.active) {
            check.missing.get_or_insert(body);
        }
        Some((call, check.missing?))
    }).collect()
}

fn initialized_const<'s, 't>(node: Node<'t>, source: &'s str) -> Option<(&'s str, Node<'t>)> {
    let declaration = node.named_child(0)?;
    let name = declaration.child_by_field_name("name")?;
    (node.kind() == "lexical_declaration" && node.named_child_count() == 1
        && node.child(0)?.kind() == "const" && name.kind() == "identifier")
        .then_some((text(name, source), declaration.child_by_field_name("value")?))
}

#[derive(Clone)]
struct State<'s> {
    aliases: HashSet<&'s str>,
    active: bool,
}

#[derive(Clone, Copy)]
enum Phase {
    Setup,
    Cleanup,
}

struct Check<'s, 't> {
    source: &'s str,
    missing: Option<Node<'t>>,
}

impl<'s, 't> Check<'s, 't> {
    // None means the path contains an operation whose effect on the timer is unknown.
    // Some(None) means execution left the current callback.
    fn statements(
        &mut self, statements: impl IntoIterator<Item = Node<'t>>, mut state: State<'s>, phase: Phase,
    ) -> Option<Option<State<'s>>> {
        for node in statements {
            let Some(next) = self.statement(node, state, phase)? else { return Some(None) };
            state = next;
        }
        Some(Some(state))
    }

    fn statement(&mut self, node: Node<'t>, mut state: State<'s>, phase: Phase) -> Option<Option<State<'s>>> {
        match node.kind() {
            "if_statement" => return self.branch(node, state, phase),
            "statement_block" => return self.scoped_block(node, state, phase),
            "return_statement" => {
                self.returned(node, state, phase)?;
                return Some(None);
            }
            "expression_statement" => {
                self.expression(node.named_child(0)?, &mut state)?;
            }
            "lexical_declaration" => {
                if let Some((name, value)) = initialized_const(node, self.source) {
                    if self.is_alias(value, &state) {
                        state.aliases.insert(name);
                    } else {
                        self.opaque(value, &state)?;
                        state.aliases.remove(name);
                    }
                } else {
                    self.opaque(node, &state)?;
                }
            }
            "comment" | "empty_statement" => {}
            _ => return None,
        }
        Some(Some(state))
    }

    fn scoped_block(&mut self, node: Node<'t>, state: State<'s>, phase: Phase) -> Option<Option<State<'s>>> {
        let outer = state.aliases.clone();
        let result = self.statements(children(node), state, phase)?;
        Some(result.map(|path| State { aliases: outer, active: path.active }))
    }

    fn branch(&mut self, node: Node<'t>, state: State<'s>, phase: Phase) -> Option<Option<State<'s>>> {
        self.opaque(node.child_by_field_name("condition")?, &state)?;
        let yes = self.statement(node.child_by_field_name("consequence")?, state.clone(), phase)?;
        let no = match node.child_by_field_name("alternative") {
            Some(other) => self.statement(other.named_child(0)?, state, phase)?,
            None => Some(state),
        };
        Some(match (yes, no) {
            (Some(mut a), Some(b)) => {
                a.active |= b.active;
                a.aliases.retain(|alias| b.aliases.contains(alias));
                Some(a)
            }
            (one, other) => one.or(other),
        })
    }

    fn returned(&mut self, node: Node<'t>, mut state: State<'s>, phase: Phase) -> Option<()> {
        let expression = children(node).find(|part| part.kind() != "comment");
        match phase {
            Phase::Setup => {
                if !state.active {
                    return Some(());
                }
                match expression {
                    Some(value) if matches!(value.kind(), "arrow_function" | "function_expression") => {
                        return self.cleanup(callback_body(value)?, state);
                    }
                    Some(value) if !matches!(text(value, self.source), "undefined" | "null") => return None,
                    Some(_) => {}
                    None => {}
                }
                self.missing.get_or_insert(node);
            }
            Phase::Cleanup => {
                if let Some(value) = expression {
                    self.expression(value, &mut state)?;
                }
                if state.active {
                    self.missing.get_or_insert(node);
                }
            }
        }
        Some(())
    }

    fn cleanup(&mut self, body: Node<'t>, mut state: State<'s>) -> Option<()> {
        if body.kind() == "statement_block" {
            if self.statements(children(body), state, Phase::Cleanup)?.is_some_and(|state| state.active) {
                self.missing.get_or_insert(body);
            }
        } else {
            self.expression(body, &mut state)?;
            if state.active {
                self.missing.get_or_insert(body);
            }
        }
        Some(())
    }

    fn expression(&self, node: Node<'t>, state: &mut State<'s>) -> Option<()> {
        if called(node, self.source, "clearInterval") {
            let target = first_argument(node)?;
            if self.is_alias(target, state) {
                state.active = false;
            } else {
                self.opaque(target, state)?;
            }
            return Some(());
        }
        self.opaque(node, state)
    }

    fn is_alias(&self, node: Node<'_>, state: &State<'s>) -> bool {
        node.kind() == "identifier" && state.aliases.contains(text(node, self.source))
    }

    fn opaque(&self, node: Node<'_>, state: &State<'s>) -> Option<()> {
        (!descendants(node).any(|part| self.is_alias(part, state))).then_some(())
    }
}

fn called(node: Node<'_>, source: &str, name: &str) -> bool {
    node.kind() == "call_expression"
        && node.child_by_field_name("function").is_some_and(|callee| {
            callee.kind() == "identifier" && text(callee, source) == name
        })
}
