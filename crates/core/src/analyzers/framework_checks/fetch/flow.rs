use std::collections::{BTreeMap, BTreeSet, VecDeque};

use autoreview_dataflow::cfg::IdentityOperand;
use autoreview_dataflow::{Cfg, CfgNode, EdgeKind, Stmt};

use super::syntax::{Binding, Call, Condition, Status, Syntax, Test};
use super::{Evidence, Issue};

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum Value {
    Response(usize),
    Json { response: usize, read: usize },
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct Response {
    status_source: usize,
    read: Option<usize>,
}

#[derive(Clone, Default, Eq, Ord, PartialEq, PartialOrd)]
struct State<'s> {
    bindings: BTreeMap<Binding<'s>, Value>,
    flags: BTreeMap<Binding<'s>, bool>,
    results: BTreeMap<usize, Value>,
    responses: BTreeMap<usize, Response>,
    statuses: BTreeMap<usize, Status>,
}

#[derive(Clone, Copy)]
enum Operand<'s> {
    Binding(Binding<'s>),
    Result(usize),
    Boolean(bool),
}

enum Step<'s> {
    Call(usize, Call<'s>),
    Bind(Binding<'s>, Option<Operand<'s>>),
    Return(Operand<'s>),
    ReturnedPromise(usize, usize),
}

struct Block<'s> {
    steps: Vec<Step<'s>>,
    condition: Option<Condition<'s>>,
}

pub(super) fn find_issues(cfg: &Cfg<Stmt>, syntax: &Syntax<'_, '_, '_>, issues: &mut Evidence) {
    const MAX_STATES_PER_NODE: usize = 64;
    const MAX_STATES_PER_FUNCTION: usize = 8_192;
    let blocks: Vec<_> = cfg
        .nodes
        .iter()
        .map(|node| Block::lower(node, syntax))
        .collect();
    let mut successors = vec![Vec::new(); cfg.nodes.len()];
    for &(source, target, edge) in &cfg.edges {
        successors[source].push((target, edge));
    }
    let mut visited = vec![BTreeSet::new(); blocks.len()];
    let mut queue = VecDeque::from([(cfg.entry, State::default())]);
    let mut results = Results {
        issues,
        returns: BTreeMap::new(),
    };
    let mut explored = 0;
    while let Some((at, mut state)) = queue.pop_front() {
        let states = &mut visited[at];
        if states.len() >= MAX_STATES_PER_NODE || !states.insert(state.clone()) {
            continue;
        }
        explored += 1;
        if explored > MAX_STATES_PER_FUNCTION {
            break;
        }
        let block = &blocks[at];
        for step in &block.steps {
            state.apply(step, &mut results);
        }
        for &(target, edge) in &successors[at] {
            let mut branch = state.clone();
            if branch.take(block.condition, edge) {
                queue.push_back((target, branch));
            }
        }
    }
}

struct Results<'a> {
    issues: &'a mut Evidence,
    returns: BTreeMap<(usize, usize), Returned>,
}

#[derive(Default)]
struct Returned {
    accepted: bool,
    rejected: bool,
}

impl Results<'_> {
    fn decoded_return(&mut self, read: usize, fetch: usize, status: Option<Status>) {
        let returned = self.returns.entry((read, fetch)).or_default();
        match status {
            Some(Status::Ok | Status::Examined) => returned.accepted = true,
            Some(Status::Error) => returned.rejected = true,
            None => {}
        }
        // Both sides of an observed status check return the same payload.
        // The check did not change how the caller receives that response.
        if status.is_none() || returned.accepted && returned.rejected {
            self.issues.entry((read, Issue::Status)).or_insert(fetch);
        }
    }
}

impl<'s> Block<'s> {
    fn lower(node: &CfgNode<Stmt>, syntax: &Syntax<'_, 's, '_>) -> Self {
        let steps = node
            .stmts
            .iter()
            .filter_map(|stmt| lower_step(stmt, node, syntax))
            .collect();
        let condition = node
            .condition_span()
            .and_then(|span| syntax.condition(span));
        Self { steps, condition }
    }
}

fn lower_step<'s>(
    stmt: &Stmt,
    node: &CfgNode<Stmt>,
    syntax: &Syntax<'_, 's, '_>,
) -> Option<Step<'s>> {
    match stmt {
        Stmt::Call {
            site: Some(site), ..
        } => Some(Step::Call(site.start, syntax.operation(site.start)?)),
        Stmt::Rebind(write) => {
            let (name, position) = write.target();
            let target = syntax.bind_at(name, position)?;
            let rhs = match write.value() {
                Some(IdentityOperand::Binding(name)) => {
                    syntax.bind_at(name, position).map(Operand::Binding)
                }
                Some(IdentityOperand::CallResult { start, .. }) => Some(Operand::Result(*start)),
                Some(IdentityOperand::Boolean(value)) => Some(Operand::Boolean(*value)),
                _ => None,
            };
            Some(Step::Bind(target, rhs))
        }
        Stmt::ReturnIdentity(value) => lower_return(value.as_ref(), node.return_start(), syntax),
        _ => None,
    }
}

fn lower_return<'s>(
    operand: Option<&IdentityOperand>,
    start: Option<usize>,
    syntax: &Syntax<'_, 's, '_>,
) -> Option<Step<'s>> {
    let location = start?;
    if let Some((read, fetch)) = syntax.returned_promise(location) {
        return Some(Step::ReturnedPromise(read, fetch));
    }
    let value = match operand? {
        IdentityOperand::CallResult { start, .. } => Operand::Result(*start),
        IdentityOperand::Binding(name) => Operand::Binding(syntax.return_binding(name, location)?),
        _ => return None,
    };
    Some(Step::Return(value))
}

impl<'s> State<'s> {
    fn apply(&mut self, step: &Step<'s>, results: &mut Results<'_>) {
        match *step {
            Step::Call(site, call) => self.call(site, call, results.issues),
            Step::Bind(target, operand) => self.bind(target, operand),
            Step::Return(operand) => self.check_status(operand, results),
            Step::ReturnedPromise(read, fetch) => {
                results.decoded_return(read, fetch, None);
            }
        }
    }

    fn bind(&mut self, target: Binding<'s>, operand: Option<Operand<'s>>) {
        let flag = match operand {
            Some(Operand::Boolean(value)) => Some(value),
            Some(Operand::Binding(binding)) => self.flags.get(&binding).copied(),
            _ => None,
        };
        if let Some(value) = flag {
            self.flags.insert(target, value);
        } else {
            self.flags.remove(&target);
        }
        if let Some(value) = operand.and_then(|operand| self.value(operand)) {
            self.bindings.insert(target, value);
        } else {
            self.bindings.remove(&target);
        }
    }

    fn value(&self, operand: Operand<'s>) -> Option<Value> {
        match operand {
            Operand::Binding(binding) => self.bindings.get(&binding).copied(),
            Operand::Result(site) => self.results.get(&site).copied(),
            Operand::Boolean(_) => None,
        }
    }

    fn response(&self, binding: Binding<'s>) -> Option<usize> {
        match self.bindings.get(&binding)? {
            Value::Response(site) => Some(*site),
            Value::Json { .. } => None,
        }
    }

    fn call(&mut self, site: usize, call: Call<'s>, issues: &mut Evidence) {
        self.results.remove(&site);
        match call {
            Call::Fetch => {
                self.statuses.remove(&site);
                self.create_response(site, site);
            }
            Call::Clone(binding) => self.clone_response(site, binding),
            Call::Read {
                receiver,
                json,
                cast,
            } => self.read(site, receiver, json, cast, issues),
        }
    }

    fn clone_response(&mut self, site: usize, binding: Binding<'s>) {
        let Some(response) = self
            .response(binding)
            .and_then(|site| self.responses.get(&site))
        else {
            return;
        };
        if response.read.is_none() {
            self.create_response(site, response.status_source);
        }
    }

    fn create_response(&mut self, site: usize, status_source: usize) {
        // Returning to an acquisition site in a loop creates a different
        // body. Stop tracking references to older instances at that site.
        self.bindings
            .retain(|_, value| response_site(*value) != site);
        self.results
            .retain(|_, value| response_site(*value) != site);
        self.responses.insert(
            site,
            Response {
                status_source,
                read: None,
            },
        );
        self.results.insert(site, Value::Response(site));
    }

    fn read(
        &mut self,
        site: usize,
        receiver: Binding<'s>,
        json: bool,
        cast: bool,
        issues: &mut Evidence,
    ) {
        let Some(id) = self.response(receiver) else {
            return;
        };
        let Some(response) = self.responses.get_mut(&id) else {
            return;
        };
        if cast {
            issues
                .entry((site, Issue::Cast))
                .or_insert(response.status_source);
        }
        if let Some(first) = response.read {
            issues.entry((site, Issue::Body)).or_insert(first);
        } else {
            response.read = Some(site);
        }
        if json {
            self.results.insert(
                site,
                Value::Json {
                    response: id,
                    read: site,
                },
            );
        }
    }

    fn check_status(&self, operand: Operand<'s>, results: &mut Results<'_>) {
        let Some(Value::Json { response, read }) = self.value(operand) else {
            return;
        };
        let Some(response) = self.responses.get(&response) else {
            return;
        };
        results.decoded_return(
            read,
            response.status_source,
            self.statuses.get(&response.status_source).copied(),
        );
    }

    fn take(&mut self, test: Option<Condition<'s>>, edge: EdgeKind) -> bool {
        let truth = match edge {
            EdgeKind::True => true,
            EdgeKind::False => false,
            _ => return true,
        };
        match test {
            Some(Condition::Flag(binding, positive)) => {
                *self.flags.entry(binding).or_insert(truth == positive) == (truth == positive)
            }
            Some(Condition::Constant(value)) => truth == value,
            Some(Condition::Response(binding, test)) => self.response_branch(binding, test, truth),
            None => true,
        }
    }

    fn response_branch(&mut self, binding: Binding<'s>, test: Test, truth: bool) -> bool {
        let Some(response) = self
            .response(binding)
            .and_then(|site| self.responses.get(&site))
        else {
            return true;
        };
        match test {
            Test::Ok { positive } => {
                let status = if truth == positive {
                    Status::Ok
                } else {
                    Status::Error
                };
                constrain_status(&mut self.statuses, response.status_source, status)
            }
            Test::Numeric { yes, no } => {
                let status = if truth { yes } else { no };
                status.is_none_or(|known| {
                    constrain_status(&mut self.statuses, response.status_source, known)
                })
            }
            Test::BodyUsed { positive } => truth == positive || response.read.is_none(),
        }
    }
}

fn constrain_status(statuses: &mut BTreeMap<usize, Status>, source: usize, known: Status) -> bool {
    if known == Status::Examined {
        statuses.entry(source).or_insert(known);
        return true;
    }
    if statuses
        .get(&source)
        .is_some_and(|old| *old != Status::Examined && *old != known)
    {
        return false;
    }
    statuses.insert(source, known);
    true
}

fn response_site(value: Value) -> usize {
    match value {
        Value::Response(site) => site,
        Value::Json { response, .. } => response,
    }
}
