use std::collections::{HashMap, HashSet};

use super::{children, descendants, location, text, AgentFinding, Node, Path, Rule};
use super::web::{self, binding_nodes, field_text, first_argument, is_function};

pub(super) const RULE: Rule = Rule {
    id: "prisma-outer-client-write-in-transaction",
    category: "correctness",
    subcategory: "persistence.transaction-boundaries",
    title: "Prisma write uses the outer client inside an interactive transaction",
    message: "This write uses the client that opened the transaction and can run outside its rollback boundary. Use the transaction callback's client for work that must roll back together.",
    reference: "https://docs.prisma.io/docs/orm/v7/prisma-client/queries/transactions#interactive-transactions",
};

pub(super) fn run(root: &Path, paths: &[String]) -> Vec<AgentFinding> {
    web::run(root, paths, |path, source, file| {
        client_bindings(file, source).map_or_else(Vec::new, |clients| writes(path, source, file, &clients))
    })
}

fn prisma_imports<'s>(root: Node<'_>, source: &'s str) -> HashSet<&'s str> {
    children(root)
        .filter(|node| node.kind() == "import_statement"
            && field_text(*node, "source", source).is_some_and(|module| module.trim_matches(['\'', '"']) == "@prisma/client"))
        .flat_map(descendants)
        .filter_map(|node| {
            (node.kind() == "import_specifier" && field_text(node, "name", source) == Some("PrismaClient"))
                .then(|| field_text(node, "alias", source).or_else(|| field_text(node, "name", source)))
                .flatten()
        })
        .collect()
}

fn client_bindings<'s>(root: Node<'_>, source: &'s str) -> Option<HashMap<&'s str, usize>> {
    let imports = prisma_imports(root, source);
    if imports.is_empty() {
        return None;
    }
    let mut clients = HashMap::new();
    let mut known_bindings = HashSet::new();
    for (name, value) in children(root).flat_map(|item| const_declarations(item, source)) {
        let identity = match value.kind() {
            "new_expression" if field_text(value, "constructor", source).is_some_and(|class| imports.contains(class)) => value.start_byte(),
            "identifier" => match clients.get(text(value, source)) {
                Some(&identity) => identity,
                None => continue,
            },
            _ => continue,
        };
        clients.insert(text(name, source), identity);
        known_bindings.insert(name.id());
    }
    for binding in descendants(root).flat_map(binding_nodes) {
        let name = text(binding, source);
        if !known_bindings.contains(&binding.id()) {
            clients.remove(name);
        }
    }
    (!clients.is_empty()).then_some(clients)
}

fn const_declarations<'t>(item: Node<'t>, source: &str) -> Vec<(Node<'t>, Node<'t>)> {
    let declaration = if item.kind() == "export_statement" {
        item.child_by_field_name("declaration")
    } else {
        Some(item)
    };
    declaration.into_iter()
        .filter(|node| node.kind() == "lexical_declaration" && field_text(*node, "kind", source) == Some("const"))
        .flat_map(children)
        .filter_map(|node| Some((
            node.child_by_field_name("name").filter(|name| name.kind() == "identifier")?,
            node.child_by_field_name("value")?,
        )))
        .collect()
}

fn writes(path: &str, source: &str, file: Node<'_>, clients: &HashMap<&str, usize>) -> Vec<AgentFinding> {
    let transactions = descendants(file).filter_map(|node| transaction(node, source, clients));
    transactions.flat_map(|(acquire, body, identity)| {
        descendants(body).filter_map(move |node| {
            if write_receiver(node, source).and_then(|receiver| clients.get(receiver)) != Some(&identity) {
                return None;
            }
            if std::iter::successors(node.parent(), |parent| parent.parent())
                .take_while(|parent| *parent != body).any(is_function)
            {
                return None;
            }
            Some(RULE.finding(location(path, node, source), vec![location(path, acquire, source)]))
        })
    }).collect()
}

fn transaction<'t>(node: Node<'t>, source: &str, clients: &HashMap<&str, usize>) -> Option<(Node<'t>, Node<'t>, usize)> {
    let function = member_call(node)?;
    if field_text(function, "property", source) != Some("$transaction") {
        return None;
    }
    let identity = *clients.get(field_text(function, "object", source)?)?;
    let callback = first_argument(node)?;
    is_function(callback).then(|| (node, callback.child_by_field_name("body").unwrap_or(callback), identity))
}

fn write_receiver<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    let function = member_call(node)?;
    let method = field_text(function, "property", source)?;
    let object = function.child_by_field_name("object")?;
    if method == "$executeRaw" {
        return Some(text(object, source));
    }
    if object.kind() != "member_expression"
        || !matches!(method, "create" | "createMany" | "createManyAndReturn" | "update" | "updateMany" | "updateManyAndReturn" | "upsert" | "delete" | "deleteMany")
    {
        return None;
    }
    field_text(object, "object", source)
}

fn member_call(node: Node<'_>) -> Option<Node<'_>> {
    (node.kind() == "call_expression")
        .then(|| node.child_by_field_name("function"))
        .flatten()
        .filter(|function| function.kind() == "member_expression")
}
