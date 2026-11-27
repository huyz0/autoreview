use super::super::is_function;
use super::{children, descendants, field_text, inside_named_expression, text, Node, NodeCalls};

impl<'t> NodeCalls<'t, '_> {
    pub fn options_object(&self, argument: Node<'t>, call: Node<'t>) -> Option<Node<'t>> {
        let object = if argument.kind() == "identifier" {
            self.local_options(argument, call)?
        } else {
            argument
        };
        (object.kind() == "object").then_some(object)
    }

    pub fn const_initializer(&self, binding: Node<'t>) -> Option<Node<'t>> {
        let name = text(binding, self.source);
        if binding.kind() != "identifier" || inside_named_expression(binding, name, self.source) {
            return None;
        }
        let scope = self.scopes.visible(name, binding)?;
        let (decl, value) = const_value(scope, name, self.source)?;
        (decl.end_byte() <= binding.start_byte()).then_some(value)
    }

    fn local_options(&self, options: Node<'t>, call: Node<'t>) -> Option<Node<'t>> {
        let name = text(options, self.source);
        if inside_named_expression(options, name, self.source) {
            return None;
        }
        let scope = self.scopes.visible(name, options)?;
        let (declaration, object) = const_value(scope, name, self.source)?;
        let statement = containing_statement(call, scope)?;
        if declaration.end_byte() > statement.start_byte()
            || descendants(scope).any(|node| {
                node.start_byte() < options.start_byte()
                    && !declaration.byte_range().contains(&node.start_byte())
                    && matches!(node.kind(), "identifier" | "shorthand_property_identifier")
                    && text(node, self.source) == name
            })
        {
            return None;
        }
        Some(object)
    }
}

fn containing_statement<'t>(call: Node<'t>, scope: Node<'t>) -> Option<Node<'t>> {
    std::iter::successors(Some(call), |node| node.parent())
        .take_while(|node| {
            !is_function(*node)
                && !matches!(
                    node.kind(),
                    "statement_block"
                        | "if_statement"
                        | "for_statement"
                        | "for_in_statement"
                        | "while_statement"
                        | "do_statement"
                        | "switch_statement"
                )
        })
        .find(|node| node.parent() == Some(scope))
}

fn const_value<'t>(scope: Node<'t>, name: &str, source: &str) -> Option<(Node<'t>, Node<'t>)> {
    let mut declarations =
        const_declarations(scope).filter(|decl| field_text(*decl, "name", source) == Some(name));
    let decl = declarations.next()?;
    (declarations.next().is_none()).then_some((decl, decl.child_by_field_name("value")?))
}

pub(super) fn const_declarations(scope: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    children(scope)
        .filter(|statement| {
            statement.kind() == "lexical_declaration"
                && statement
                    .child(0)
                    .is_some_and(|token| token.kind() == "const")
        })
        .flat_map(children)
}
