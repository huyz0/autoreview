mod guards;
mod workload;

use std::path::Path;

use anyhow::ensure;
use autoreview_schema::artifact_config::{
    ExpressRoutePolicy, ImportedOperation, RouteAuthorization, RouteSharedResource,
};
use autoreview_schema::AgentFinding;

use super::super::artifact_policy::Rule;
use super::web::express::inline_route;
use super::web::imported_operations::FileCalls;
use super::web::node_calls::NodeCalls;
use super::web::scoped_policy::{self, Scope};
use super::{descendants, location, Node};
use guards::{Check, Handler, ResultUse};

pub(in crate::analyzers) const AUTHORIZATION: Rule = Rule {
    id: "express-route-entity-verification-required",
    category: "security",
    subcategory: "authorization.object-scope",
    tool: "autoreview-express-route-policy",
    title: "Protected entity operation lacks the required verifier guard",
    reference: "https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html",
    enabled_by: "artifactChecks.expressRoutePolicies",
};

pub(in crate::analyzers) const RACE: Rule = Rule {
    id: "express-route-shared-resource-check-then-act",
    category: "security",
    subcategory: "concurrency.shared-resource",
    tool: "autoreview-express-route-policy",
    title: "Shared resource is checked and changed in separate operations",
    reference: "https://cwe.mitre.org/data/definitions/362.html",
    ..AUTHORIZATION
};

pub(in crate::analyzers) use workload::LIMIT;

pub fn run_express_route_policy(
    root: &Path,
    changed: &[String],
    configs: &[ExpressRoutePolicy],
) -> anyhow::Result<Vec<AgentFinding>> {
    scoped_policy::run(root, changed, configs.iter().map(compile), inspect)
}

fn compile(route: &ExpressRoutePolicy) -> anyhow::Result<Scope<'_, ExpressRoutePolicy>> {
    ensure!(
        !route.paths.is_empty() && route.route.starts_with('/'),
        "Express route policy needs source paths and an absolute route pattern"
    );
    ensure!(
        ["get", "post", "put", "patch", "delete", "all"]
            .iter()
            .any(|method| route.method.eq_ignore_ascii_case(method)),
        "unsupported Express route policy method {}",
        route.method
    );
    ensure!(
        route
            .workload
            .as_ref()
            .is_none_or(|policy| { !policy.limit_field.is_empty() && policy.maximum > 0 }),
        "Express route workload policy needs a limit field and positive maximum"
    );
    Scope::new(&route.paths, route)
}

fn inspect(
    path: &str,
    source: &str,
    root: Node<'_>,
    policies: &[Scope<'_, ExpressRoutePolicy>],
) -> Vec<AgentFinding> {
    let calls = NodeCalls::new(root, source);
    let operations = FileCalls::new(root, source);
    let mut findings = Vec::new();
    for route in descendants(root).filter_map(|call| inline_route(call, &calls, source)) {
        let Some(handler) = Handler::new(route.handler, &calls, source) else {
            continue;
        };
        let review = Review {
            path,
            source,
            handler,
            operations: &operations,
        };
        for policy in policies.iter().filter(|policy| {
            policy.matches(path)
                && policy.config.route == route.path
                && policy.config.method.eq_ignore_ascii_case(route.method)
        }) {
            findings.extend(review.route_findings(policy.config));
        }
    }
    findings
}

struct Review<'a, 't, 's> {
    path: &'s str,
    source: &'s str,
    handler: Handler<'a, 't, 's>,
    operations: &'a FileCalls<'t, 's>,
}

impl<'t> Review<'_, 't, '_> {
    fn route_findings(&self, route: &ExpressRoutePolicy) -> Vec<AgentFinding> {
        let mut findings = Vec::new();
        if let Some(policy) = &route.authorization {
            findings.extend(self.authorization_findings(policy));
        }
        if let Some(policy) = &route.workload {
            findings.extend(workload::findings(self, policy));
        }
        if let Some(policy) = &route.shared_resource {
            findings.extend(self.race_findings(policy));
        }
        findings
    }

    fn direct_calls(&self, operation: &ImportedOperation) -> Vec<Node<'t>> {
        let calls = self.operations.matching_calls(operation);
        self.handler
            .direct_calls()
            .filter(|call| calls.contains_key(&call.start_byte()))
            .collect()
    }

    fn authorization_findings(&self, policy: &RouteAuthorization) -> Vec<AgentFinding> {
        let verifiers = self.operations.matching_calls(&policy.verifier);
        let verifier = Check {
            calls: &verifiers,
            key_argument: policy.verifier_key_argument,
        };
        self.direct_calls(&policy.protected_operation).into_iter().filter_map(|call| {
            let key = self.handler.key(call, policy.operation_key_argument)?;
            if self.handler.prior_check(call, key, &verifier, ResultUse::DenialGuard).is_some() {
                return None;
            }
            let message = format!(
                "This operation on {} requires a prior guard on the registered verifier result for the same key.",
                policy.entity,
            );
            let mut finding = self.finding(&AUTHORIZATION, call, message);
            if let Some(verifier) = self.handler.prior_check(call, key, &verifier, ResultUse::DirectCall) {
                finding.related_locations = Some(vec![location(self.path, verifier, self.source)]);
            }
            Some(finding)
        }).collect()
    }

    fn race_findings(&self, policy: &RouteSharedResource) -> Vec<AgentFinding> {
        let checks = self.operations.matching_calls(&policy.check);
        let resource_check = Check {
            calls: &checks,
            key_argument: policy.check_key_argument,
        };
        self.direct_calls(&policy.mutation).into_iter().filter_map(|call| {
            let key = self.handler.key(call, policy.mutation_key_argument)?;
            let check = self.handler.prior_check(call, key, &resource_check, ResultUse::DenialGuard)?;
            let message = format!(
                "The check and mutation of {} use the same key in separate calls. Use an atomic operation or a transaction that covers both.",
                policy.resource,
            );
            let mut finding = self.finding(&RACE, call, message);
            finding.related_locations = Some(vec![location(self.path, check, self.source)]);
            Some(finding)
        }).collect()
    }

    fn finding(&self, rule: &Rule, call: Node<'t>, message: String) -> AgentFinding {
        let anchor = location(self.path, call, self.source);
        rule.finding(self.path, anchor.range, &anchor.snippet, message)
    }
}
