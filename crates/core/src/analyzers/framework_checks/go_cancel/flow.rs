use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use super::super::{children, descendants, text};

pub(super) type Helpers = HashMap<String, (usize, bool)>;

pub(super) struct Check<'a, 'tree> {
    pub source: &'a str,
    pub binding: &'a str,
    pub acquire: Option<Node<'tree>>,
    pub helpers: &'a Helpers,
    shadowed: &'a HashSet<&'a str>,
    pub exit: Option<Node<'tree>>,
    pub used: Vec<String>,
}

const CLEAR: u8 = 1;
const OPEN: u8 = 2;

impl<'tree> Check<'_, 'tree> {
    pub fn check(&mut self, body: Node<'tree>) -> Option<bool> {
        let live = self.block(body, if self.acquire.is_some() { CLEAR } else { OPEN })?;
        self.record_exit(body, live);
        Some(self.exit.is_none())
    }

    fn block(&mut self, node: Node<'tree>, mut live: u8) -> Option<u8> {
        let Some(list) = children(node).find(|part| part.kind() == "statement_list") else {
            return Some(live);
        };
        for item in children(list).take_while(|part| part.kind() != "ERROR") {
            if live == 0 {
                break;
            }
            live = self.statement(item, live)?;
        }
        Some(live)
    }

    fn statement(&mut self, node: Node<'tree>, live: u8) -> Option<u8> {
        if self.acquire == Some(node) {
            return Some(OPEN);
        }
        match node.kind() {
            "block" => self.block(node, live),
            "if_statement" => self.branch(node, live),
            "return_statement" => self.returned(node, live),
            "defer_statement" | "expression_statement" => {
                let Some(call) = node
                    .named_child(0)
                    .filter(|part| part.kind() == "call_expression")
                else {
                    return self.opaque(node, live);
                };
                self.call(call, live)
            }
            "for_statement"
            | "expression_switch_statement"
            | "type_switch_statement"
            | "select_statement"
            | "labeled_statement"
            | "goto_statement" => None,
            _ => self.opaque(node, live),
        }
    }

    fn branch(&mut self, node: Node<'tree>, mut live: u8) -> Option<u8> {
        if let Some(initializer) = node.child_by_field_name("initializer") {
            live = self.statement(initializer, live)?;
        }
        if let Some(condition) = node.child_by_field_name("condition") {
            live = self.opaque(condition, live)?;
        }
        let yes = self.block(node.child_by_field_name("consequence")?, live)?;
        let no = match node.child_by_field_name("alternative") {
            Some(other) => self.statement(other, live)?,
            None => live,
        };
        Some(yes | no)
    }

    fn returned(&mut self, node: Node<'tree>, live: u8) -> Option<u8> {
        let transfers = node
            .named_child(0)
            .is_some_and(|list| children(list).any(|part| self.is_binding(part)));
        if !transfers {
            self.record_exit(node, self.opaque(node, live)?);
        } else if self.acquire.is_none() {
            // Returning the argument does not prove a helper consumes it.
            self.record_exit(node, live);
        }
        Some(0)
    }

    fn record_exit(&mut self, node: Node<'tree>, live: u8) {
        if live & OPEN != 0 && self.exit.is_none() {
            self.exit = Some(node);
        }
    }

    fn call(&mut self, call: Node<'tree>, live: u8) -> Option<u8> {
        let callee = call.child_by_field_name("function")?;
        if self.is_binding(callee) {
            return Some(CLEAR);
        }
        let name = text(callee, self.source);
        let Some(&(position, consumes)) = self
            .helpers
            .get(name)
            .filter(|_| !self.shadowed.contains(name))
        else {
            return self.opaque(call, live);
        };
        let arguments = call.child_by_field_name("arguments")?;
        if children(arguments)
            .nth(position)
            .is_some_and(|argument| self.is_binding(argument))
        {
            self.used.push(name.to_owned());
            return Some(if consumes { CLEAR } else { live });
        }
        self.opaque(call, live)
    }

    fn opaque(&self, node: Node<'_>, live: u8) -> Option<u8> {
        (!descendants(node).any(|part| self.is_binding(part))).then_some(live)
    }

    fn is_binding(&self, node: Node<'_>) -> bool {
        node.kind() == "identifier" && text(node, self.source) == self.binding
    }
}

impl<'a, 'tree> Check<'a, 'tree> {
    pub fn new(
        source: &'a str,
        binding: &'a str,
        acquire: Option<Node<'tree>>,
        helpers: &'a Helpers,
        shadowed: &'a HashSet<&'a str>,
    ) -> Self {
        Self {
            source,
            binding,
            acquire,
            helpers,
            shadowed,
            exit: None,
            used: Vec::new(),
        }
    }
}
