use std::collections::{BTreeMap, HashMap};

use super::super::{children, descendants, location, text, AgentFinding, Node, Rule};
use super::package::File;
use super::span::aliases;

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "go-sql-db-write-during-transaction",
    category: "correctness",
    subcategory: "persistence.transaction-boundaries",
    title: "SQL write uses the parent database during a transaction",
    message: "This Exec call runs through the database that opened the active transaction. Use the transaction handle for writes that must commit or roll back with it.",
    reference: "https://go.dev/doc/database/execute-transactions",
};

#[derive(Clone, Copy)]
enum Value {
    Database(usize),
    Transaction(usize),
    BeginError(usize),
}

#[derive(Clone, Default)]
struct State<'s, 't> {
    values: HashMap<&'s str, Value>,
    transactions: BTreeMap<usize, (usize, Node<'t>)>,
}

struct Check<'s, 't> {
    source: &'s str,
    hits: BTreeMap<usize, (Node<'t>, Node<'t>)>,
}

pub(super) fn findings(file: &File) -> Vec<AgentFinding> {
    let sql = aliases(file, "database/sql");
    if sql.is_empty() {
        return Vec::new();
    }
    let source = &file.source;
    let mut check = Check { source, hits: BTreeMap::new() };
    for function in file.functions() {
        let Some(body) = function.child_by_field_name("body") else { continue };
        let state = parameters(function, &sql, source);
        if !state.values.is_empty() {
            check.block(body, state);
        }
    }
    check.hits.into_values().map(|(write, begin)| {
        RULE.finding(location(&file.path, write, source), vec![location(&file.path, begin, source)])
    }).collect()
}

fn parameters<'s, 't>(function: Node<'t>, sql: &std::collections::HashSet<&str>, source: &'s str) -> State<'s, 't> {
    let mut state = State::default();
    for param in function.child_by_field_name("parameters").into_iter().flat_map(children) {
        let qualified = param.child_by_field_name("type").filter(|ty| ty.kind() == "pointer_type")
            .and_then(|ty| ty.named_child(0));
        let database = qualified.is_some_and(|ty| {
            ty.child_by_field_name("package").is_some_and(|package| sql.contains(text(package, source)))
                && ty.child_by_field_name("name").is_some_and(|name| text(name, source) == "DB")
        });
        if database {
            for (index, node) in children(param).enumerate() {
                if param.field_name_for_named_child(index as u32) == Some("name") {
                    state.values.insert(text(node, source), Value::Database(node.start_byte()));
                }
            }
        }
    }
    state
}

impl<'s, 't> Check<'s, 't> {
    fn block(&mut self, block: Node<'t>, state: State<'s, 't>) -> Vec<State<'s, 't>> {
        let mut states = vec![state];
        let mut outer = HashMap::new();
        let Some(statements) = children(block).find(|node| node.kind() == "statement_list") else { return states };
        for node in children(statements) {
            if states.is_empty() || states.len() > 24 {
                return Vec::new();
            }
            states = self.advance_block(node, states, &mut outer);
        }
        for (index, state) in states.iter_mut().enumerate() {
            for (&name, values) in &outer {
                set(&mut state.values, name, values[index]);
            }
        }
        states
    }

    fn advance_block(
        &mut self, node: Node<'t>, states: Vec<State<'s, 't>>,
        outer: &mut HashMap<&'s str, Vec<Option<Value>>>,
    ) -> Vec<State<'s, 't>> {
        // Restore only names introduced inside the block. Plain assignments
        // can still update an outer variable.
        for name in short_names(node, self.source) {
            outer.entry(name).or_insert_with(|| {
                states.iter().map(|state| state.values.get(name).copied()).collect()
            });
        }
        let previous_count = states.len();
        let mut origins = Vec::new();
        let states = states.into_iter().enumerate().flat_map(|(index, state)| {
            let next = self.statement(node, state);
            origins.extend(std::iter::repeat_n(index, next.len()));
            next
        }).collect();
        if origins.len() != previous_count || origins.iter().copied().ne(0..previous_count) {
            for values in outer.values_mut() {
                *values = origins.iter().map(|index| values[*index]).collect();
            }
        }
        states
    }

    fn statement(&mut self, node: Node<'t>, mut state: State<'s, 't>) -> Vec<State<'s, 't>> {
        match node.kind() {
            "block" => return self.block(node, state),
            "if_statement" => return self.branch(node, state),
            "return_statement" => {
                // A returned Exec can be the escaping write.
                if let Some(result) = node.named_child(0) {
                    self.expression_calls(result, &mut state);
                }
                return Vec::new();
            }
            "short_var_declaration" | "assignment_statement" => self.assignment(node, &mut state),
            "expression_statement" => self.expression_calls(node, &mut state),
            "defer_statement" | "comment" | "empty_statement" => {}
            _ => return Vec::new(),
        }
        vec![state]
    }

    fn branch(&mut self, node: Node<'t>, mut state: State<'s, 't>) -> Vec<State<'s, 't>> {
        // Initializers have their own scope. Until that scope is lowered,
        // decline to carry transaction state through it.
        if node.child_by_field_name("initializer").is_some() {
            return Vec::new();
        }
        let Some(condition) = node.child_by_field_name("condition") else { return Vec::new() };
        self.expression_calls(condition, &mut state);
        let mut yes = state.clone();
        if let Some((transaction, true_is_failure)) = self.error_branch(condition, &state) {
            if true_is_failure {
                yes.transactions.remove(&transaction);
            } else {
                state.transactions.remove(&transaction);
            }
        }
        let mut paths = node.child_by_field_name("consequence").map_or_else(Vec::new, |body| self.block(body, yes));
        let no = match node.child_by_field_name("alternative") {
            Some(other) => self.statement(other, state),
            None => vec![state],
        };
        paths.extend(no);
        paths
    }

    fn error_branch(&self, condition: Node<'_>, state: &State<'s, 't>) -> Option<(usize, bool)> {
        let left = condition.child_by_field_name("left")?;
        let right = condition.child_by_field_name("right")?;
        let error = if right.kind() == "nil" { left } else if left.kind() == "nil" { right } else { return None };
        let Value::BeginError(transaction) = *state.values.get(text(error, self.source))? else { return None };
        match condition.child_by_field_name("operator").map(|operator| text(operator, self.source)) {
            Some("!=") => Some((transaction, true)),
            Some("==") => Some((transaction, false)),
            _ => None,
        }
    }

    fn assignment(&mut self, node: Node<'t>, state: &mut State<'s, 't>) {
        let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else { return };
        let names: Vec<_> = children(left).map(|name| text(name, self.source)).collect();
        let mut values: Vec<_> = children(right).map(|value| {
            state.values.get(text(value, self.source)).copied()
        }).collect();
        self.expression_calls(right, state);
        if names.len() == 2 && right.named_child_count() == 1 {
            if let Some(call) = right.named_child(0) {
                if let Some((receiver, "Begin" | "BeginTx")) = receiver_call(call, self.source) {
                    if let Some(Value::Database(owner)) = state.values.get(receiver).copied() {
                        let id = call.start_byte();
                        state.transactions.insert(id, (owner, call));
                        values = vec![Some(Value::Transaction(id)), Some(Value::BeginError(id))];
                    }
                }
            }
        }
        for (index, name) in names.into_iter().enumerate() {
            set(&mut state.values, name, values.get(index).copied().flatten());
        }
    }

    fn expression_calls(&mut self, expression: Node<'t>, state: &mut State<'s, 't>) {
        let mut calls: Vec<_> = descendants(expression).filter(|node| {
            node.kind() == "call_expression"
                && !std::iter::successors(node.parent(), |parent| parent.parent())
                    .take_while(|parent| parent.start_byte() >= expression.start_byte())
                    .any(|parent| parent.kind() == "func_literal")
        }).collect();
        calls.sort_by_key(|call| call.end_byte());
        for call in calls {
            self.call(call, state);
        }
    }

    fn call(&mut self, call: Node<'t>, state: &mut State<'s, 't>) {
        match receiver_call(call, self.source).and_then(|(name, method)| Some((*state.values.get(name)?, method))) {
            Some((Value::Database(database), "Exec" | "ExecContext")) => {
                if let Some((_, begin)) = state.transactions.values().find(|(owner, _)| *owner == database) {
                    self.hits.entry(call.start_byte()).or_insert((call, *begin));
                }
            }
            Some((Value::Transaction(id), "Commit" | "Rollback")) => { state.transactions.remove(&id); }
            _ => {}
        }
        // An opaque helper receiving a transaction could terminate it.
        for argument in call.child_by_field_name("arguments").into_iter().flat_map(children) {
            if let Some(Value::Transaction(id)) = state.values.get(text(argument, self.source)) {
                state.transactions.remove(id);
            }
        }
    }
}

fn short_names<'s, 't>(node: Node<'t>, source: &'s str) -> impl Iterator<Item = &'s str> + use<'s, 't> {
    (node.kind() == "short_var_declaration").then(|| node.child_by_field_name("left")).flatten()
        .into_iter().flat_map(children).map(|name| text(name, source))
}

fn set<'s>(values: &mut HashMap<&'s str, Value>, name: &'s str, value: Option<Value>) {
    if name != "_" {
        if let Some(value) = value { values.insert(name, value); } else { values.remove(name); }
    }
}

fn receiver_call<'s>(call: Node<'_>, source: &'s str) -> Option<(&'s str, &'s str)> {
    let function = call.child_by_field_name("function").filter(|node| node.kind() == "selector_expression")?;
    let receiver = function.child_by_field_name("operand").filter(|node| node.kind() == "identifier")?;
    Some((text(receiver, source), text(function.child_by_field_name("field")?, source)))
}
