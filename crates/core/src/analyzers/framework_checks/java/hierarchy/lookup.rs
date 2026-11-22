use std::collections::HashSet;

use tree_sitter::Node;

use super::super::Java;
use super::model::{
    parameter_type, simple_type, type_children, Hierarchy, Method, Transaction, Type, Visibility,
};

#[derive(Clone, Copy)]
pub(in crate::analyzers::framework_checks::java) struct Candidate<'a> {
    pub owner: &'a Type,
    pub method: &'a Method,
}

pub(in crate::analyzers::framework_checks::java) struct Lookup<'a> {
    hierarchy: &'a Hierarchy,
    owner: &'a Type,
    member_types: Members<'a>,
    super_types: Option<Members<'a>>,
}

#[derive(Default)]
struct Members<'a> {
    types: Vec<(&'a Type, bool)>,
    missing: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Receiver {
    Current,
    Super,
}

impl<'a> Lookup<'a> {
    pub fn new(hierarchy: &'a Hierarchy, owner: &'a Type) -> Self {
        Self {
            hierarchy,
            owner,
            member_types: Members::load(hierarchy, owner, &owner.package),
            super_types: owner
                .superclass
                .as_deref()
                .and_then(|parent| hierarchy.named(owner, parent))
                .map(|parent| Members::load(hierarchy, parent, &owner.package)),
        }
    }

    pub fn resolve(
        &self,
        java: &Java<'_>,
        callable: Node<'_>,
        call: Node<'_>,
        allow_super: bool,
    ) -> Option<Candidate<'a>> {
        let (members, local) = match receiver(call)? {
            Receiver::Current => (&self.member_types, Some(self.owner.name.as_str())),
            Receiver::Super if allow_super => (self.super_types.as_ref()?, None),
            Receiver::Super => return None,
        };
        let name = java.field(call, "name")?;
        let arguments: Vec<_> = type_children(call.child_by_field_name("arguments")?).collect();
        let candidates = members
            .methods(name, local)
            .filter(|candidate| candidate.accepts_arity(arguments.len()));
        let selected = self.hierarchy.distinct(candidates);
        let exact = || {
            if selected
                .iter()
                .any(|candidate| self.hierarchy.parameters(*candidate).is_none())
            {
                return None;
            }
            let types = arguments
                .iter()
                .map(|arg| {
                    argument_type(java, callable, *arg)
                        .and_then(|raw| self.hierarchy.canonical(self.owner, &raw))
                })
                .collect::<Option<Vec<_>>>()?;
            let mut exact = selected.iter().copied().filter(|candidate| {
                self.hierarchy
                    .parameters(*candidate)
                    .is_some_and(|params| params == types)
            });
            let matched = exact.next()?;
            exact.next().is_none().then_some(matched)
        };
        if selected
            .iter()
            .any(|candidate| candidate.method.varargs || candidate.method.generic)
        {
            return None;
        }
        match selected.as_slice() {
            [target] if !members.missing => Some(*target),
            _ => exact()
                .filter(|target| !members.missing || local == Some(target.owner.name.as_str())),
        }
    }

    pub fn declared(&self, java: &Java<'_>, node: Node<'_>) -> Option<Candidate<'a>> {
        let name = java.field(node, "name")?;
        let method = self
            .owner
            .methods
            .iter()
            .find(|method| method.name == name && method.start == node.start_byte())?;
        Some(Candidate {
            owner: self.owner,
            method,
        })
    }
}

impl<'a> Candidate<'a> {
    pub fn overridable(self) -> bool {
        self.method.visibility != Visibility::Private
            && !self.method.is_static
            && !self.method.is_final
    }

    fn accepts_arity(self, arguments: usize) -> bool {
        let declared = self.method.parameters.len();
        if self.method.varargs {
            arguments >= declared.saturating_sub(1)
        } else {
            arguments == declared
        }
    }
}

impl<'a> Members<'a> {
    fn load(hierarchy: &'a Hierarchy, start: &'a Type, package: &str) -> Self {
        let mut members = Self::default();
        let mut seen = HashSet::new();
        let mut pending = vec![(start, true)];
        while let Some((owner, through_package)) = pending.pop() {
            if !seen.insert(&owner.name) {
                continue;
            }
            let same_package = through_package && owner.package == package;
            members.types.push((owner, same_package));
            for parent in owner.superclass.iter().chain(&owner.interfaces).rev() {
                if let Some(parent) = hierarchy.named(owner, parent) {
                    pending.push((parent, same_package));
                } else {
                    members.missing = true;
                }
            }
        }
        members
    }

    fn methods<'b>(
        &'b self,
        name: &'b str,
        local: Option<&'b str>,
    ) -> impl Iterator<Item = Candidate<'a>> + 'b {
        self.types.iter().flat_map(move |&(owner, same_package)| {
            owner
                .methods
                .iter()
                .filter(move |method| {
                    method.name == name && accessible(owner, method, local, same_package)
                })
                .map(move |method| Candidate { owner, method })
        })
    }
}

fn accessible(owner: &Type, method: &Method, local: Option<&str>, same_package: bool) -> bool {
    if local == Some(owner.name.as_str()) {
        return true;
    }
    match method.visibility {
        Visibility::Private => false,
        Visibility::Package => same_package,
        Visibility::Inherited => !owner.is_interface || !method.is_static,
    }
}

impl Hierarchy {
    fn parameters(&self, candidate: Candidate<'_>) -> Option<Vec<String>> {
        candidate
            .method
            .parameters
            .iter()
            .map(|raw| self.canonical(candidate.owner, raw.as_deref()?))
            .collect()
    }

    fn distinct<'a>(&self, methods: impl Iterator<Item = Candidate<'a>>) -> Vec<Candidate<'a>> {
        let mut selected: Vec<Candidate<'a>> = Vec::new();
        for candidate in methods {
            let signature = self.parameters(candidate);
            let overlaps = |existing: Candidate<'_>| {
                signature.is_some() && signature == self.parameters(existing)
            };
            if selected
                .iter()
                .copied()
                .any(|earlier| overlaps(earlier) && self.shadows(earlier.owner, candidate.owner))
            {
                continue;
            }
            selected.retain(|&earlier| {
                !overlaps(earlier) || !self.shadows(candidate.owner, earlier.owner)
            });
            selected.push(candidate);
        }
        selected
    }

    fn shadows(&self, closer: &Type, farther: &Type) -> bool {
        if closer.name == farther.name {
            return false;
        }
        (!closer.is_interface && farther.is_interface)
            || Members::load(self, closer, &closer.package)
                .types
                .iter()
                .any(|(ancestor, _)| ancestor.name == farther.name)
    }

    pub fn transaction<'a>(&'a self, selected: Candidate<'a>) -> Option<&'a Transaction> {
        selected
            .method
            .transaction
            .as_ref()
            .or_else(|| self.inherited_transaction(selected))
            .or_else(|| self.type_transaction(selected.owner))
    }

    fn inherited_transaction<'a>(&'a self, selected: Candidate<'a>) -> Option<&'a Transaction> {
        if !selected.overridable() || selected.method.generic || selected.method.varargs {
            return None;
        }
        let signature = self.parameters(selected)?;
        let members = Members::load(self, selected.owner, &selected.owner.package);
        let methods = members
            .methods(&selected.method.name, None)
            .filter(|candidate| {
                candidate.owner.name != selected.owner.name
                    && candidate.overridable()
                    && candidate.method.transaction.is_some()
                    && self
                        .parameters(*candidate)
                        .is_some_and(|parameters| parameters == signature)
            });
        let nearest = self.distinct(methods);
        match nearest.as_slice() {
            [candidate] => candidate.method.transaction.as_ref(),
            _ => None,
        }
    }

    fn type_transaction<'a>(&'a self, mut owner: &'a Type) -> Option<&'a Transaction> {
        let mut seen = HashSet::new();
        while seen.insert(&owner.name) {
            if owner.transaction.is_some() {
                return owner.transaction.as_ref();
            }
            owner = self.named(owner, owner.superclass.as_deref()?)?;
        }
        None
    }
}

fn receiver(call: Node<'_>) -> Option<Receiver> {
    match call
        .child_by_field_name("object")
        .map(|object| object.kind())
    {
        None | Some("this") => Some(Receiver::Current),
        Some("super") => Some(Receiver::Super),
        _ => None,
    }
}

fn argument_type(java: &Java<'_>, callable: Node<'_>, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "string_literal" => Some("java.lang.String".into()),
        "character_literal" => Some("char".into()),
        "true" | "false" => Some("boolean".into()),
        "decimal_integer_literal"
        | "hex_integer_literal"
        | "octal_integer_literal"
        | "binary_integer_literal" => Some(
            if java.text(node).ends_with(['L', 'l']) {
                "long"
            } else {
                "int"
            }
            .into(),
        ),
        "decimal_floating_point_literal" | "hex_floating_point_literal" => Some(
            if java.text(node).ends_with(['F', 'f']) {
                "float"
            } else {
                "double"
            }
            .into(),
        ),
        "cast_expression" => simple_type(java, node.child_by_field_name("type")?),
        "identifier" => argument_parameter_type(java, callable, node),
        _ => None,
    }
}

fn argument_parameter_type(
    java: &Java<'_>,
    callable: Node<'_>,
    identifier: Node<'_>,
) -> Option<String> {
    let name = java.text(identifier);
    // Deferred or nested code may have a different binding environment.
    if std::iter::successors(identifier.parent(), |node| node.parent())
        .take_while(|node| *node != callable)
        .any(|node| matches!(node.kind(), "lambda_expression" | "class_body"))
    {
        return None;
    }
    let param = type_children(callable.child_by_field_name("parameters")?).find(|node| {
        node.kind() == "formal_parameter" && java.field(*node, "name") == Some(name)
    })?;
    parameter_type(java, param)
}
