mod flow;
mod syntax;

use std::collections::BTreeMap;
use std::path::Path;

use autoreview_langsupport::{language_for_path, Language};
use autoreview_schema::Severity;

use super::web::{is_function, BindingScopes};
use super::{descendants, location, AgentFinding, Node, Rule};
use syntax::Syntax;

pub(super) const RULES: &[Rule] = &[
    Rule {
        id: "javascript-fetch-response-body-read-twice",
        ..RULES_BODY
    },
    Rule {
        id: "typescript-fetch-response-body-read-twice",
        ..RULES_BODY
    },
    Rule {
        id: "javascript-fetch-json-without-status-check",
        ..RULES_STATUS
    },
    Rule {
        id: "typescript-fetch-json-without-status-check",
        ..RULES_STATUS
    },
    Rule {
        id: "typescript-json-cast-without-validation",
        category: "correctness",
        subcategory: "validation.runtime-boundaries",
        title: "Fetch JSON payload asserted as a domain type",
        message: "This assertion gives the decoded Fetch payload a domain type without validating its runtime shape.",
        reference: "https://www.typescriptlang.org/docs/handbook/2/everyday-types.html#type-assertions",
    },
];
const RULES_BODY: Rule = Rule {
    id: "",
    category: "correctness",
    subcategory: "protocol.response-lifecycle",
    title: "Fetch response body consumed twice",
    message: "Both reads can consume the same Fetch response. Reuse the decoded value or clone the response before either read.",
    reference: "https://fetch.spec.whatwg.org/#body-unusable",
};
const RULES_STATUS: Rule = Rule {
    id: "",
    category: "correctness",
    subcategory: "protocol.status-handling",
    title: "Fetch JSON returned without checking the response status",
    message: "This path returns decoded JSON without handling the response status. Fetch also resolves for HTTP error responses.",
    reference: "https://fetch.spec.whatwg.org/#dom-response-ok",
};

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum Issue {
    Body,
    Status,
    Cast,
}

type Evidence = BTreeMap<(usize, Issue), usize>;

pub(super) fn inspect(path: &str, source: &str, root: Node<'_>) -> Vec<AgentFinding> {
    if !source.contains("fetch") {
        return Vec::new();
    }
    let Some(language) = language_for_path(Path::new(path)) else {
        return Vec::new();
    };
    let scopes = BindingScopes::new(root, source);
    let syntax = Syntax::new(root, source, &scopes);
    let mut issues = BTreeMap::new();
    for function in descendants(root).filter(|function| is_function(*function)) {
        if !syntax.has_fetch(function) {
            continue;
        }
        let cfg = autoreview_dataflow::lower::keyed_variant(language, source.as_bytes(), function);
        flow::find_issues(&cfg, &syntax, &mut issues);
    }
    issues
        .into_iter()
        .filter_map(|((primary, issue), related)| {
            let call = match issue {
                Issue::Cast => syntax.assertion_at(primary)?,
                _ => syntax.call_at(primary)?,
            };
            let evidence = syntax
                .call_at(related)
                .filter(|node| *node != call)
                .map(|node| location(path, node, source));
            let mut finding = rule(issue, language)
                .finding(location(path, call, source), evidence.into_iter().collect());
            if issue == Issue::Status {
                finding.severity = Severity::Info;
            }
            Some(finding)
        })
        .collect()
}

fn rule(issue: Issue, language: Language) -> &'static Rule {
    let offset = usize::from(language != Language::JavaScript);
    &RULES[match issue {
        Issue::Body => offset,
        Issue::Status => 2 + offset,
        Issue::Cast => 4,
    }]
}
