use std::collections::{HashMap, HashSet};

use super::web::{binding_nodes, field_text, first_argument, imports_from, is_function};
use super::{children, descendants, location, text, AgentFinding, Node, Rule};

pub(super) const RULE: Rule = Rule {
    id: "rabbitmq-ack-in-finally",
    category: "correctness",
    subcategory: "messaging.acknowledgements",
    title: "RabbitMQ acknowledgement runs even if processing fails",
    message: "This finally block acknowledges the delivery after either successful or failed processing. Acknowledge success explicitly and choose reject or requeue behavior for failures.",
    reference: "https://www.rabbitmq.com/docs/confirms#consumer-acks",
};

pub(super) fn inspect(path: &str, source: &str, root: Node<'_>) -> Vec<AgentFinding> {
    let modules = imported_modules(root, source);
    if modules.is_empty() {
        return Vec::new();
    }
    let initializers = initializers(root, source);
    let mut findings = Vec::new();
    for consume in descendants(root).filter(|node| node.kind() == "call_expression") {
        let Some((channel, "consume")) = member_call(consume, source) else { continue };
        if !amqp_channel(channel, consume, source, &modules, &initializers) {
            continue;
        }
        let Some((message, body)) = consumer(consume, source) else { continue };
        findings.extend(consumer_acks(body, source, channel, message).into_iter().map(|(ack, processing)| {
            RULE.finding(location(path, ack, source), vec![location(path, processing, source)])
        }));
    }
    findings
}

fn consumer_acks<'t>(
    body: Node<'t>, source: &str, channel: &str, message: &str,
) -> Vec<(Node<'t>, Node<'t>)> {
    if descendants(body).flat_map(binding_nodes).any(|node| {
        let name = text(node, source);
        name == message || name == channel
    }) {
        return Vec::new();
    }
    descendants(body).filter(|node| node.kind() == "try_statement" && in_callback(*node, body))
        .filter_map(processing_and_finally)
        .flat_map(|(processing, finally)| {
            children(finally).filter_map(|statement| statement.named_child(0))
                .filter(|&ack| acknowledges(ack, source, channel, message))
                .map(move |ack| (ack, processing))
        }).collect()
}

fn processing_and_finally(attempt: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    let processing = attempt.child_by_field_name("body").filter(|block| may_fail(*block))?;
    let finally = attempt.child_by_field_name("finalizer")?.child_by_field_name("body")?;
    Some((processing, finally))
}

fn acknowledges(ack: Node<'_>, source: &str, channel: &str, message: &str) -> bool {
    member_call(ack, source) == Some((channel, "ack"))
        && first_argument(ack).is_some_and(|argument| {
            argument.kind() == "identifier" && text(argument, source) == message
        })
}

fn imported_modules<'s>(root: Node<'_>, source: &'s str) -> HashSet<&'s str> {
    imports_from(root, source, "amqplib").flat_map(descendants)
        .filter(|node| node.kind() == "namespace_import")
        .filter_map(|node| node.named_child(0))
        .map(|node| text(node, source))
        .collect()
}

type Initializers<'s, 't> = HashMap<&'s str, Option<Node<'t>>>;

fn initializers<'s, 't>(root: Node<'t>, source: &'s str) -> Initializers<'s, 't> {
    let mut bindings = HashMap::new();
    for binding in descendants(root).flat_map(binding_nodes) {
        let declaration = binding.parent().filter(|parent| parent.kind() == "variable_declarator");
        let initializer = declaration.filter(|node| {
            node.parent().and_then(|parent| field_text(parent, "kind", source)) == Some("const")
        }).and_then(|node| node.child_by_field_name("value"));
        bindings.entry(text(binding, source)).and_modify(|value| *value = None).or_insert(initializer);
    }
    bindings
}

fn amqp_channel(
    channel: &str, call: Node<'_>, source: &str, modules: &HashSet<&str>, initializers: &Initializers<'_, '_>,
) -> bool {
    let Some((connection, "createChannel" | "createConfirmChannel")) = initializer_call(channel, call, source, initializers) else {
        return false;
    };
    initializer_call(connection, call, source, initializers).is_some_and(|(module, method)| {
        method == "connect" && modules.contains(module) && !initializers.contains_key(module)
    })
}

fn initializer_call<'s>(name: &str, use_site: Node<'_>, source: &'s str, initializers: &Initializers<'_, '_>) -> Option<(&'s str, &'s str)> {
    let expression = initializers.get(name)?.as_ref()?;
    let declaration = expression.parent()?.parent()?;
    let scope = declaration.parent()?;
    if expression.end_byte() > use_site.start_byte()
        || !(scope.start_byte() <= use_site.start_byte() && use_site.end_byte() <= scope.end_byte())
    {
        return None;
    }
    if expression.kind() != "await_expression" {
        return None;
    }
    member_call(expression.named_child(0)?, source)
}

fn consumer<'s, 't>(consume: Node<'t>, source: &'s str) -> Option<(&'s str, Node<'t>)> {
    let mut args = children(consume.child_by_field_name("arguments")?).filter(|node| node.kind() != "comment");
    args.next()?;
    let callback = args.next().filter(|node| is_function(*node))?;
    if let Some(options) = args.next() {
        if options.kind() != "object" || children(options).any(|option| {
            option.kind() != "pair" || field_text(option, "key", source)
                .is_some_and(|key| key.trim_matches(['\'', '"']) == "noAck")
                && field_text(option, "value", source) != Some("false")
        }) {
            return None;
        }
    }
    let parameter = callback.child_by_field_name("parameter").or_else(|| callback.child_by_field_name("parameters")?.named_child(0))?;
    let binding = if parameter.kind() == "required_parameter" { parameter.child_by_field_name("pattern")? } else { parameter };
    (binding.kind() == "identifier").then_some((text(binding, source), callback.child_by_field_name("body")?))
}

fn member_call<'s>(call: Node<'_>, source: &'s str) -> Option<(&'s str, &'s str)> {
    let method = call.child_by_field_name("function").filter(|function| function.kind() == "member_expression")?;
    let object = method.child_by_field_name("object").filter(|node| node.kind() == "identifier")?;
    Some((text(object, source), field_text(method, "property", source)?))
}

fn in_callback(node: Node<'_>, body: Node<'_>) -> bool {
    std::iter::successors(node.parent(), |parent| parent.parent())
        .take_while(|parent| *parent != body).all(|parent| !is_function(parent))
}

fn may_fail(body: Node<'_>) -> bool {
    descendants(body).any(|node| matches!(node.kind(), "call_expression" | "throw_statement") && in_callback(node, body))
}
