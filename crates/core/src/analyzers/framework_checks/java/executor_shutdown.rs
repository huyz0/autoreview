mod flow;

use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::Rule;
use super::{children, class_members, containing, descendants, Java};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "java-executor-shutdown-hook-without-await",
    category: "correctness",
    subcategory: "reliability.shutdown",
    title: "JVM shutdown hook can exit before executor tasks complete",
    message: "ExecutorService.shutdown starts an orderly shutdown but returns immediately. Wait for termination in this hook or close the executor on supported JDKs.",
    reference: "https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/concurrent/ExecutorService.html#shutdown()",
};

pub(super) fn findings(java: &Java<'_>) -> Vec<AgentFinding> {
    descendants(java.root)
        .filter_map(|call| Some((call, hook_callback(java, call)?)))
        .flat_map(|(hook, callback)| {
            incomplete_shutdowns(java, callback)
                .into_iter()
                .map(move |shutdown| {
                    RULE.finding(java.location(shutdown), vec![java.location(hook)])
                })
        })
        .collect()
}

fn hook_callback<'t>(java: &Java<'t>, hook: Node<'t>) -> Option<Node<'t>> {
    let runtime = jdk_call(java, hook, "addShutdownHook")?;
    let class = jdk_call(java, runtime, "getRuntime")?;
    if !jdk_type(java, class, "java.lang.Runtime") {
        return None;
    }
    let thread = arguments(hook)?.next()?;
    if thread.kind() != "object_creation_expression"
        || !thread
            .child_by_field_name("type")
            .is_some_and(|ty| jdk_type(java, ty, "java.lang.Thread"))
    {
        return None;
    }
    arguments(thread)?
        .find(|argument| matches!(argument.kind(), "lambda_expression" | "method_reference"))
}

fn jdk_call<'t>(java: &Java<'t>, call: Node<'t>, name: &str) -> Option<Node<'t>> {
    (call.kind() == "method_invocation" && java.field(call, "name") == Some(name))
        .then(|| call.child_by_field_name("object"))
        .flatten()
}

fn arguments(call: Node<'_>) -> Option<impl Iterator<Item = Node<'_>>> {
    Some(
        children(call.child_by_field_name("arguments")?)
            .filter(|node| !matches!(node.kind(), "line_comment" | "block_comment")),
    )
}

fn jdk_type(java: &Java<'_>, ty: Node<'_>, qualified: &str) -> bool {
    let spelling = java.text(ty);
    if java.resolves(spelling, qualified) {
        return true;
    }
    let Some(short) = qualified.strip_prefix("java.lang.") else {
        return false;
    };
    spelling == short
        && !java.imports.contains_key(short)
        && !descendants(java.root).any(|node| {
            matches!(
                node.kind(),
                "class_declaration" | "record_declaration" | "interface_declaration"
            ) && java.field(node, "name") == Some(short)
        })
}

fn incomplete_shutdowns<'t>(java: &Java<'t>, callback: Node<'t>) -> Vec<Node<'t>> {
    if callback.kind() == "method_reference" {
        return shutdown_reference(java, callback).into_iter().collect();
    }
    let Some(body) = callback.child_by_field_name("body") else {
        return Vec::new();
    };
    if body.kind() == "method_invocation" {
        return executor_call(java, body, callback)
            .into_iter()
            .filter(|call| call.method == "shutdown")
            .map(|call| call.node)
            .collect();
    }
    let calls: Vec<_> = descendants(body)
        .filter_map(|call| executor_call(java, call, callback))
        .collect();
    if !calls.iter().any(|call| call.method == "shutdown") {
        return Vec::new();
    }
    let pending = flow::unfinished(java, callback, &calls);
    calls
        .into_iter()
        .filter(|call| pending.contains(&call.node.start_byte()))
        .map(|call| call.node)
        .collect()
}

struct ExecutorCall<'t> {
    node: Node<'t>,
    method: &'t str,
    identity: usize,
}

fn executor_call<'t>(
    java: &Java<'t>,
    call: Node<'t>,
    callback: Node<'t>,
) -> Option<ExecutorCall<'t>> {
    let method = java.field(call, "name")?;
    if call.kind() != "method_invocation"
        || !matches!(
            method,
            "shutdown" | "shutdownNow" | "close" | "awaitTermination"
        )
        || nested_callable(call, callback)
    {
        return None;
    }
    Some(ExecutorCall {
        node: call,
        method,
        identity: receiver_identity(java, call.child_by_field_name("object")?)?,
    })
}

fn shutdown_reference<'t>(java: &Java<'t>, callback: Node<'t>) -> Option<Node<'t>> {
    let mut parts = children(callback);
    let receiver = parts.next()?;
    let method = parts.last()?;
    (java.text(method) == "shutdown" && receiver_identity(java, receiver).is_some())
        .then_some(callback)
}

fn nested_callable(call: Node<'_>, callback: Node<'_>) -> bool {
    std::iter::successors(call.parent(), |node| node.parent())
        .take_while(|node| *node != callback)
        .any(|node| {
            matches!(
                node.kind(),
                "lambda_expression" | "method_declaration" | "class_body"
            )
        })
}

fn receiver_identity<'t>(java: &Java<'t>, receiver: Node<'t>) -> Option<usize> {
    let declaration = match receiver.kind() {
        "identifier" | "type_identifier" => {
            visible_declaration(java, receiver, java.text(receiver))
        }
        "field_access" if java.field(receiver, "object") == Some("this") => {
            declared_field(java, receiver, java.field(receiver, "field")?)
        }
        _ => None,
    }?;
    executor_type(java, declaration).then_some(declaration.0.start_byte())
}

fn visible_declaration<'t>(
    java: &Java<'t>,
    receiver: Node<'t>,
    name: &str,
) -> Option<(Node<'t>, Node<'t>)> {
    let callable = std::iter::successors(receiver.parent(), |node| node.parent()).find(|node| {
        matches!(
            node.kind(),
            "method_declaration" | "constructor_declaration" | "static_initializer"
        )
    });
    let locals = callable
        .into_iter()
        .flat_map(descendants)
        .filter(|node| {
            matches!(
                node.kind(),
                "local_variable_declaration" | "formal_parameter"
            )
        })
        .filter_map(|declaration| local_binding(java, receiver, declaration, name))
        .max_by_key(|(binding, _)| binding.start_byte());
    locals.or_else(|| declared_field(java, receiver, name))
}

fn local_binding<'t>(
    java: &Java<'t>,
    receiver: Node<'t>,
    declaration: Node<'t>,
    name: &str,
) -> Option<(Node<'t>, Node<'t>)> {
    if declaration.start_byte() > receiver.start_byte() {
        return None;
    }
    let scope = std::iter::successors(declaration.parent(), |node| node.parent()).find(|node| {
        matches!(
            node.kind(),
            "block"
                | "method_declaration"
                | "constructor_declaration"
                | "lambda_expression"
                | "for_statement"
                | "catch_clause"
        )
    })?;
    if !(scope.start_byte() <= receiver.start_byte() && receiver.end_byte() <= scope.end_byte()) {
        return None;
    }
    declared_variable(java, declaration, name)
}

fn declared_field<'t>(
    java: &Java<'t>,
    receiver: Node<'t>,
    name: &str,
) -> Option<(Node<'t>, Node<'t>)> {
    let class = containing(receiver, "class_declaration")?;
    class_members(class)
        .filter(|field| field.kind() == "field_declaration")
        .find_map(|field| declared_variable(java, field, name))
}

fn declared_variable<'t>(
    java: &Java<'t>,
    declaration: Node<'t>,
    name: &str,
) -> Option<(Node<'t>, Node<'t>)> {
    let key = if declaration.kind() == "formal_parameter" {
        declaration
            .child_by_field_name("name")
            .filter(|node| java.text(*node) == name)
    } else {
        children(declaration).find(|node| {
            node.kind() == "variable_declarator" && java.field(*node, "name") == Some(name)
        })
    }?;
    Some((key, declaration.child_by_field_name("type")?))
}

fn executor_type(java: &Java<'_>, (declaration, ty): (Node<'_>, Node<'_>)) -> bool {
    if java.text(ty) == "var" {
        return declaration
            .child_by_field_name("value")
            .is_some_and(|value| executor_factory(java, value));
    }
    [
        "java.util.concurrent.ExecutorService",
        "java.util.concurrent.ScheduledExecutorService",
        "java.util.concurrent.ThreadPoolExecutor",
        "java.util.concurrent.ScheduledThreadPoolExecutor",
        "java.util.concurrent.ForkJoinPool",
    ]
    .iter()
    .any(|name| java.resolves(java.text(ty), name))
}

fn executor_factory(java: &Java<'_>, call: Node<'_>) -> bool {
    const FACTORIES: &[&str] = &[
        "newCachedThreadPool",
        "newFixedThreadPool",
        "newScheduledThreadPool",
        "newSingleThreadExecutor",
        "newSingleThreadScheduledExecutor",
        "newThreadPerTaskExecutor",
        "newVirtualThreadPerTaskExecutor",
        "newWorkStealingPool",
    ];
    call.kind() == "method_invocation"
        && java
            .field(call, "name")
            .is_some_and(|name| FACTORIES.contains(&name))
        && java
            .field(call, "object")
            .is_some_and(|object| java.resolves(object, "java.util.concurrent.Executors"))
}
