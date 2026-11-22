use std::collections::HashSet;
use std::ops::Range;

use super::super::web::{field_text, first_argument, named_imports, BindingScopes};
use super::super::{children, descendants, text, Node};

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Binding<'s> {
    owner: usize,
    name: &'s str,
}

#[derive(Clone, Copy)]
pub(super) enum Call<'s> {
    Fetch,
    Clone(Binding<'s>),
    Read {
        receiver: Binding<'s>,
        json: bool,
        cast: bool,
    },
}

#[derive(Clone, Copy)]
pub(super) enum Test {
    Ok {
        positive: bool,
    },
    Numeric {
        yes: Option<Status>,
        no: Option<Status>,
    },
    BodyUsed {
        positive: bool,
    },
}

#[derive(Clone, Copy)]
pub(super) enum Condition<'s> {
    Flag(Binding<'s>, bool),
    Constant(bool),
    Response(Binding<'s>, Test),
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Status {
    Ok,
    Error,
    Examined,
}

pub(super) struct Syntax<'t, 's, 'a> {
    root: Node<'t>,
    source: &'s str,
    scopes: &'a BindingScopes<'t, 's>,
    known_imports: HashSet<&'s str>,
    imports: HashSet<&'s str>,
}

impl<'t, 's, 'a> Syntax<'t, 's, 'a> {
    pub fn new(root: Node<'t>, source: &'s str, scopes: &'a BindingScopes<'t, 's>) -> Self {
        let imports = children(root)
            .filter(|node| node.kind() == "import_statement")
            .flat_map(imported_names)
            .map(|node| text(node, source))
            .collect();
        let known_imports = named_imports(root, source, "undici")
            .filter_map(|(name, local)| (name == "fetch").then_some(local))
            .chain(
                super::super::web::imports_from(root, source, "node-fetch")
                    .filter_map(default_import)
                    .map(|node| text(node, source)),
            )
            .collect();
        Self {
            root,
            source,
            scopes,
            known_imports,
            imports,
        }
    }

    pub fn has_fetch(&self, function: Node<'t>) -> bool {
        descendants(function).any(|node| self.is_fetch(node))
    }

    pub fn call_at(&self, start: usize) -> Option<Node<'t>> {
        self.node_at(start, "call_expression")
    }

    pub fn bind_at<'n>(&self, name: &'n str, site: usize) -> Option<Binding<'s>> {
        let leaf = self.root.descendant_for_byte_range(site, site + 1)?;
        let text = text(leaf, self.source);
        // AST and CFG names both come from source. Locate the source slice
        // for an alias operand whose node may be inside this statement.
        let name = if text == name {
            text
        } else {
            self.name_at(name, leaf)?
        };
        Some(self.binding(name, leaf))
    }

    pub fn return_binding(&self, name: &str, start: usize) -> Option<Binding<'s>> {
        let value = unwrapped(self.node_at(start, "return_statement")?.named_child(0)?)?;
        (value.kind() == "identifier" && text(value, self.source) == name)
            .then(|| self.binding(text(value, self.source), value))
    }

    pub fn operation(&self, start: usize) -> Option<Call<'s>> {
        let call = self.call_at(start)?;
        if self.is_fetch(call) {
            return awaited(call).then_some(Call::Fetch);
        }
        let (receiver, method) = self.response_member(call.child_by_field_name("function")?)?;
        match method {
            "clone" => Some(Call::Clone(receiver)),
            "arrayBuffer" | "blob" | "bytes" | "formData" | "json" | "text" => Some(Call::Read {
                receiver,
                json: method == "json",
                cast: method == "json" && domain_assertion(call).is_some(),
            }),
            _ => None,
        }
    }

    pub fn assertion_at(&self, start: usize) -> Option<Node<'t>> {
        domain_assertion(self.call_at(start)?)
    }

    pub fn condition(&self, span: Range<usize>) -> Option<Condition<'s>> {
        let node = unwrapped(self.root.descendant_for_byte_range(span.start, span.end)?)?;
        let mut positive = true;
        let mut current = node;
        while current.kind() == "unary_expression"
            && field_text(current, "operator", self.source) == Some("!")
        {
            positive = !positive;
            current = unwrapped(current.child_by_field_name("argument")?)?;
        }
        match current.kind() {
            "binary_expression" => {
                let (binding, test) = self.status_comparison(current, positive)?;
                return Some(Condition::Response(binding, test));
            }
            "identifier" => {
                let binding = self.binding(text(current, self.source), current);
                return Some(Condition::Flag(binding, positive));
            }
            "true" => return Some(Condition::Constant(positive)),
            "false" => return Some(Condition::Constant(!positive)),
            _ => {}
        }
        let (binding, field) = self.response_member(current)?;
        let test = match field {
            "ok" => Test::Ok { positive },
            "bodyUsed" => Test::BodyUsed { positive },
            _ => return None,
        };
        Some(Condition::Response(binding, test))
    }

    pub fn returned_promise(&self, start: usize) -> Option<(usize, usize)> {
        let value = unwrapped(self.node_at(start, "return_statement")?.named_child(0)?)?;
        let method = value.child_by_field_name("function")?;
        if field_text(method, "property", self.source) != Some("then") {
            return None;
        }
        let fetch = method
            .child_by_field_name("object")?
            .filter_kind("call_expression")?;
        if !self.is_fetch(fetch) {
            return None;
        }
        let callback = first_argument(value)?.filter_kind("arrow_function")?;
        let parameter = callback
            .child_by_field_name("parameter")
            .or_else(|| callback.child_by_field_name("parameters")?.named_child(0))?;
        let parameter = parameter
            .child_by_field_name("pattern")
            .unwrap_or(parameter);
        let body = unwrapped(callback.child_by_field_name("body")?)?;
        let (receiver, "json") = self.response_member(body.child_by_field_name("function")?)?
        else {
            return None;
        };
        (text(parameter, self.source) == receiver.name)
            .then_some((body.start_byte(), fetch.start_byte()))
    }

    fn binding(&self, name: &'s str, at: Node<'_>) -> Binding<'s> {
        Binding {
            name,
            owner: self.scopes.visible(name, at).map_or(0, |scope| scope.id()),
        }
    }

    fn name_at(&self, name: &str, leaf: Node<'t>) -> Option<&'s str> {
        let assignment = std::iter::successors(Some(leaf), |node| node.parent())
            .find(|node| matches!(node.kind(), "assignment_expression" | "variable_declarator"))?;
        descendants(assignment)
            .find(|node| node.kind() == "identifier" && text(*node, self.source) == name)
            .map(|node| text(node, self.source))
    }

    fn response_member(&self, node: Node<'t>) -> Option<(Binding<'s>, &'s str)> {
        let node = node.filter_kind("member_expression")?;
        let receiver = node
            .child_by_field_name("object")?
            .filter_kind("identifier")?;
        let method = field_text(node, "property", self.source)?;
        Some((self.binding(text(receiver, self.source), receiver), method))
    }

    fn status_comparison(&self, node: Node<'t>, positive: bool) -> Option<(Binding<'s>, Test)> {
        let (receiver, "status") = self.response_member(node.child_by_field_name("left")?)? else {
            return None;
        };
        let status: u16 = field_text(node, "right", self.source)?.parse().ok()?;
        let op = field_text(node, "operator", self.source)?;
        let (yes, no) = compared_status(op, status)?;
        let (yes, no) = if positive { (yes, no) } else { (no, yes) };
        Some((receiver, Test::Numeric { yes, no }))
    }

    fn is_fetch(&self, call: Node<'t>) -> bool {
        let Some(callee) = call
            .filter_kind("call_expression")
            .and_then(|call| call.child_by_field_name("function"))
        else {
            return false;
        };
        if callee.kind() == "identifier" {
            let name = text(callee, self.source);
            return self.scopes.visible(name, callee).is_none()
                && (self.known_imports.contains(name)
                    || name == "fetch" && !self.imports.contains(name));
        }
        field_text(callee, "property", self.source) == Some("fetch")
            && callee.child_by_field_name("object").is_some_and(|owner| {
                let name = text(owner, self.source);
                matches!(name, "globalThis" | "window")
                    && self.scopes.visible(name, owner).is_none()
                    && !self.imports.contains(name)
            })
    }

    fn node_at(&self, start: usize, kind: &str) -> Option<Node<'t>> {
        let leaf = self.root.descendant_for_byte_range(start, start + 1)?;
        std::iter::successors(Some(leaf), |node| node.parent())
            .find(|node| node.start_byte() == start && node.kind() == kind)
    }
}

fn compared_status(op: &str, status: u16) -> Option<(Option<Status>, Option<Status>)> {
    let exact = Some(if status < 300 {
        Status::Ok
    } else {
        Status::Error
    });
    let supported_code = (200..600).contains(&status);
    let error = Some(Status::Error);
    let accepted = Some(Status::Examined);
    match op {
        "==" | "===" if supported_code => Some((exact, None)),
        "!=" | "!==" if supported_code => Some((None, exact)),
        ">=" if matches!(status, 300 | 400) => Some((error, accepted)),
        "<" if matches!(status, 300 | 400) => Some((accepted, error)),
        ">" if matches!(status, 299 | 399) => Some((error, accepted)),
        "<=" if matches!(status, 299 | 399) => Some((accepted, error)),
        _ => None,
    }
}

fn default_import(node: Node<'_>) -> Option<Node<'_>> {
    children(node)
        .find(|node| node.kind() == "import_clause")?
        .named_child(0)?
        .filter_kind("identifier")
}

fn imported_names(node: Node<'_>) -> Vec<Node<'_>> {
    default_import(node)
        .into_iter()
        .chain(descendants(node).filter_map(|node| {
            match node.kind() {
                "namespace_import" => node.named_child(0),
                "import_specifier" => node
                    .child_by_field_name("alias")
                    .or_else(|| node.child_by_field_name("name")),
                _ => None,
            }
        }))
        .collect()
}

fn unwrapped(mut node: Node<'_>) -> Option<Node<'_>> {
    while matches!(
        node.kind(),
        "parenthesized_expression" | "await_expression" | "as_expression" | "satisfies_expression"
    ) {
        node = children(node).find(|child| child.kind() != "comment")?;
    }
    Some(node)
}

fn awaited(node: Node<'_>) -> bool {
    std::iter::successors(node.parent(), |node| node.parent())
        .find(|node| !matches!(node.kind(), "parenthesized_expression" | "as_expression"))
        .is_some_and(|node| node.kind() == "await_expression")
}

fn domain_assertion(call: Node<'_>) -> Option<Node<'_>> {
    std::iter::successors(call.parent(), |node| node.parent())
        .take_while(|node| {
            matches!(
                node.kind(),
                "await_expression" | "parenthesized_expression" | "as_expression"
            )
        })
        .find(|node| {
            node.kind() == "as_expression"
                && node.named_child(1).is_some_and(|ty| {
                    matches!(
                        ty.kind(),
                        "type_identifier"
                            | "generic_type"
                            | "array_type"
                            | "nested_type_identifier"
                    )
                })
        })
}

trait NodeKind<'t> {
    fn filter_kind(self, kind: &str) -> Option<Node<'t>>;
}

impl<'t> NodeKind<'t> for Node<'t> {
    fn filter_kind(self, kind: &str) -> Option<Node<'t>> {
        (self.kind() == kind).then_some(self)
    }
}
