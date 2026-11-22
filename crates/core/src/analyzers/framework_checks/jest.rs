mod restore;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::super::artifact_policy::read_snapshot_file;
use super::web::{binding_nodes, field_text, is_function, named_imports};
use super::{children, descendants, location, text, AgentFinding, Node, Rule};

pub(super) const RULE: Rule = Rule {
    id: "jest-spy-not-restored",
    category: "correctness",
    subcategory: "testing.mock-lifecycle",
    title: "Jest spy can outlive its test",
    message: "This spy replaces a shared method and can remain installed after the test exits. Restore it in cleanup, use scoped disposal, or configure Jest to restore mocks.",
    reference: "https://jestjs.io/docs/jest-object#jestspyonobject-methodname",
};

pub(super) fn inspect(repo: &Path, path: &str, source: &str, root: Node<'_>) -> Vec<AgentFinding> {
    let shadowed: HashSet<_> = descendants(root).flat_map(binding_nodes).map(|node| text(node, source)).collect();
    let imports: HashMap<_, _> = named_imports(root, source, "@jest/globals")
        .filter(|(_, local)| !shadowed.contains(local)).collect();
    let Some(jest) = imports.get("jest").copied() else { return Vec::new() };
    let tests: Vec<_> = [imports.get("test"), imports.get("it")].into_iter().flatten().copied().collect();
    if tests.is_empty() || config_owns_restoration(repo, Path::new(path)) {
        return Vec::new();
    }
    let hook = imports.get("afterEach").copied();
    let mut findings = Vec::new();
    for test in descendants(root).filter(|node| node.kind() == "call_expression") {
        let Some(body) = test_body(test, &tests, source) else { continue };
        if hook.is_some_and(|hook| restoring_hook(test, source, hook, jest)) {
            continue;
        }
        let steps: Vec<_> = children(body).collect();
        for (index, &statement) in steps.iter().enumerate() {
            let Some((binding, spy)) = spy_declaration(statement, jest, source) else { continue };
            if restore::open_exit(&steps[index + 1..], binding, jest, source) == Some(true) {
                findings.push(RULE.finding(location(path, spy, source), vec![location(path, body, source)]));
            }
        }
    }
    findings
}

fn test_body<'t>(call: Node<'t>, test_names: &[&str], source: &str) -> Option<Node<'t>> {
    let callee = call.child_by_field_name("function")?;
    let direct = test_names.contains(&text(callee, source));
    let modifier = callee.kind() == "member_expression"
        && field_text(callee, "object", source).is_some_and(|object| test_names.contains(&object))
        && matches!(field_text(callee, "property", source), Some("only" | "concurrent"));
    (direct || modifier).then_some(())?;
    let callback = children(call.child_by_field_name("arguments")?)
        .filter(|node| node.kind() != "comment").nth(1)?;
    is_function(callback).then_some(())?;
    callback.child_by_field_name("body").filter(|body| body.kind() == "statement_block")
}

fn spy_declaration<'s, 't>(statement: Node<'t>, jest: &str, source: &'s str) -> Option<(&'s str, Node<'t>)> {
    let (name, initializer) = restore::const_binding(statement, source)?;
    if member_call(initializer, source) == Some((jest, "spyOn")) {
        Some((name, initializer))
    } else {
        None
    }
}

fn restoring_hook(test: Node<'_>, source: &str, hook: &str, jest: &str) -> bool {
    std::iter::successors(test.parent(), |node| node.parent())
        .filter(|scope| matches!(scope.kind(), "program" | "statement_block"))
        .flat_map(children)
        .filter(|statement| statement.kind() == "expression_statement")
        .filter_map(|statement| statement.named_child(0))
        .filter(|call| field_text(*call, "function", source) == Some(hook))
        .filter_map(|call| children(call.child_by_field_name("arguments")?).next())
        .filter_map(|function| function.child_by_field_name("body"))
        .any(|body| {
            if body.kind() == "statement_block" {
                restore::open_exit(&children(body).collect::<Vec<_>>(), "", jest, source) == Some(false)
            } else {
                member_call(body, source) == Some((jest, "restoreAllMocks"))
            }
        })
}

fn config_owns_restoration(repo: &Path, test: &Path) -> bool {
    let Ok(repo) = repo.canonicalize() else { return true };
    for directory in test.parent().into_iter().flat_map(Path::ancestors) {
        let json = directory.join("jest.config.json");
        if let Some(source) = read_snapshot_file(&repo, &json) {
            return serde_json::from_str(&source).map_or(true, |config| configured_restoration(&config));
        }
        if ["jest.config.js", "jest.config.cjs", "jest.config.mjs", "jest.config.ts", "jest.config.cts", "jest.config.mts"]
            .into_iter().any(|name| repo.join(directory.join(name)).is_file())
        {
            // Dynamic configuration may enable restoration. Review never executes it.
            return true;
        }
        if let Some(source) = read_snapshot_file(&repo, &directory.join("package.json")) {
            let Ok(package) = serde_json::from_str::<serde_json::Value>(&source) else { return true };
            if let Some(config) = package.get("jest") {
                return configured_restoration(config);
            }
        }
    }
    false
}

fn configured_restoration(config: &serde_json::Value) -> bool {
    config.get("restoreMocks").and_then(serde_json::Value::as_bool)
        .unwrap_or_else(|| config.get("preset").is_some() || config.get("projects").is_some())
}

fn member_call<'s>(call: Node<'_>, source: &'s str) -> Option<(&'s str, &'s str)> {
    let callee = call.child_by_field_name("function").filter(|node| node.kind() == "member_expression")?;
    Some((field_text(callee, "object", source)?, field_text(callee, "property", source)?))
}
