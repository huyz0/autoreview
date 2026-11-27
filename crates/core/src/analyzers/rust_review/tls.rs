use autoreview_schema::Severity;
use tree_sitter::Node;

use super::basics::call_path;
use super::syntax::{
    ancestors, arguments, attributes, descendants, is_execution_boundary, method_call, Source,
};
use super::{Review, RustReviewRule};

const CERTIFICATES: RustReviewRule = RustReviewRule {
    id: "rust-tls-invalid-certificates",
    category: "security",
    severity: Severity::Medium,
    semantic: false,
    title: "Certificate chain verification disabled on TLS client",
    message: "The final builder settings allow invalid TLS certificates. Keep certificate verification enabled and add a trusted root for private services.",
    reference: "https://cwe.mitre.org/data/definitions/295.html",
    cwe: Some("CWE-295"),
};
const HOSTNAMES: RustReviewRule = RustReviewRule {
    id: "rust-tls-invalid-hostnames",
    title: "TLS client allows a mismatched peer hostname",
    message: "The final builder settings allow a certificate issued for a different hostname. Keep hostname verification enabled and use a certificate issued for the connection hostname.",
    reference: "https://cwe.mitre.org/data/definitions/297.html",
    cwe: Some("CWE-297"),
    ..CERTIFICATES
};
pub(super) const RULES: &[&RustReviewRule] = &[&CERTIFICATES, &HOSTNAMES];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Builder {
    Reqwest,
    NativeTls,
}

pub(super) fn check<'t>(review: &mut Review<'t, '_>, build: Node<'t>) {
    let Some((receiver, method)) = method_call(build) else {
        return;
    };
    if review.source.text(method) != "build" || arguments(build).next().is_some() {
        return;
    }
    let Some((builder, setters)) = builder_setters(&review.source, receiver, build) else {
        return;
    };
    let Some([certificates, hostnames]) = final_settings(&review.source, builder, setters) else {
        return;
    };
    for (rule, flag) in [(&CERTIFICATES, certificates), (&HOSTNAMES, hostnames)] {
        if let Some(setting) = flag {
            review.report_related(rule, setting, &[method]);
        }
    }
}

fn final_settings<'t>(
    source: &Source<'t, '_>,
    builder: Builder,
    setters: Vec<(Node<'t>, Node<'t>)>,
) -> Option<[Option<Node<'t>>; 2]> {
    let mut certificates = None;
    let mut hostnames = None;
    // Calls are in reverse execution order. An unknown value in the last
    // setter also masks a known value set earlier.
    for (call, method) in setters {
        let slot = match (builder, source.text(method)) {
            (_, "danger_accept_invalid_certs")
            | (Builder::Reqwest, "tls_danger_accept_invalid_certs") => &mut certificates,
            (_, "danger_accept_invalid_hostnames")
            | (Builder::Reqwest, "tls_danger_accept_invalid_hostnames") => &mut hostnames,
            (builder, name) if preserves_tls_flags(builder, name) => continue,
            _ => return None,
        };
        slot.get_or_insert_with(|| true_argument(source, call).then_some(method));
    }
    Some([certificates.flatten(), hostnames.flatten()])
}

fn true_argument(source: &Source<'_, '_>, call: Node<'_>) -> bool {
    let mut args = arguments(call);
    args.next()
        .is_some_and(|arg| arg.kind() == "boolean_literal" && source.text(arg) == "true")
        && args.next().is_none()
}

fn builder_setters<'t>(
    source: &Source<'t, '_>,
    receiver: Node<'t>,
    build: Node<'t>,
) -> Option<(Builder, Vec<(Node<'t>, Node<'t>)>)> {
    let mut setters = Vec::new();
    let stem = peel(receiver, &mut setters);
    if stem.kind() != "identifier" {
        return Some((constructor(source, stem)?, setters));
    }

    let declaration = source.local_binding(stem)?;
    let block = declaration.parent().filter(|part| part.kind() == "block")?;
    let statement = std::iter::once(build)
        .chain(ancestors(build))
        .take_while(|part| !is_execution_boundary(*part))
        .find(|part| part.parent() == Some(block))?;
    if attrs_may_skip(declaration) || attrs_may_skip(statement) {
        return None;
    }
    let mut initializer_setters = Vec::new();
    let constructor_call = peel(
        declaration.child_by_field_name("value")?,
        &mut initializer_setters,
    );
    let builder = constructor(source, constructor_call)?;
    let name = source.text(stem).trim_start_matches("r#");
    if mentions_before(source, statement, name, stem.start_byte()) {
        return None;
    }
    let prior_uses = std::iter::successors(statement.prev_named_sibling(), |part| {
        part.prev_named_sibling()
    })
    .take_while(|part| *part != declaration)
    .filter(|part| mentions_before(source, *part, name, part.end_byte()));
    for use_site in prior_uses {
        if builder != Builder::NativeTls {
            return None;
        }
        setters.extend(standalone_native_setters(source, use_site, name)?);
    }
    setters.extend(initializer_setters);
    Some((builder, setters))
}

fn standalone_native_setters<'t>(
    source: &Source<'t, '_>,
    statement: Node<'t>,
    name: &str,
) -> Option<Vec<(Node<'t>, Node<'t>)>> {
    if statement.kind() != "expression_statement" || attrs_may_skip(statement) {
        return None;
    }
    let mut setters = Vec::new();
    let target = peel(statement.named_child(0)?, &mut setters);
    if target.kind() != "identifier"
        || source.text(target).trim_start_matches("r#") != name
        || setters.is_empty()
        || setters
            .iter()
            .flat_map(|(call, _)| arguments(*call))
            .any(|arg| mentions_before(source, arg, name, arg.end_byte()))
    {
        return None;
    }
    Some(setters)
}

fn peel<'t>(mut receiver: Node<'t>, setters: &mut Vec<(Node<'t>, Node<'t>)>) -> Node<'t> {
    while let Some((next, method)) = method_call(receiver) {
        setters.push((receiver, method));
        receiver = next;
    }
    receiver
}

fn constructor<'t>(source: &Source<'t, '_>, call: Node<'t>) -> Option<Builder> {
    if arguments(call).next().is_some() {
        return None;
    }
    match call_path(source, call)?.as_str() {
        "reqwest::Client::builder"
        | "reqwest::blocking::Client::builder"
        | "reqwest::ClientBuilder::new"
        | "reqwest::blocking::ClientBuilder::new" => Some(Builder::Reqwest),
        "native_tls::TlsConnector::builder" => Some(Builder::NativeTls),
        _ => None,
    }
}

fn mentions_before(source: &Source<'_, '_>, node: Node<'_>, name: &str, before: usize) -> bool {
    descendants(node).any(|part| {
        part.start_byte() < before
            && part.kind() == "identifier"
            && source.text(part).trim_start_matches("r#") == name
    })
}

fn attrs_may_skip(statement: Node<'_>) -> bool {
    attributes(statement).next().is_some()
}

fn preserves_tls_flags(builder: Builder, method: &str) -> bool {
    match builder {
        Builder::Reqwest => matches!(
            method,
            "timeout"
                | "connect_timeout"
                | "read_timeout"
                | "pool_idle_timeout"
                | "pool_max_idle_per_host"
                | "user_agent"
                | "default_headers"
                | "identity"
                | "add_root_certificate"
                | "https_only"
                | "min_tls_version"
                | "max_tls_version"
                | "tls_built_in_root_certs"
                | "tls_certs_merge"
                | "tls_certs_only"
                | "no_proxy"
                | "proxy"
                | "tcp_nodelay"
                | "http1_only"
        ),
        Builder::NativeTls => matches!(
            method,
            "identity"
                | "add_root_certificate"
                | "disable_built_in_roots"
                | "use_sni"
                | "min_protocol_version"
                | "max_protocol_version"
                | "request_alpns"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::analyze_file;

    #[test]
    fn reports_only_final_literal_downgrades_on_resolved_builders() {
        let source = r#"
use reqwest::blocking::Client as Blocking;
use native_tls::TlsConnector;

fn builders(flag: bool) {
    reqwest::Client::builder().danger_accept_invalid_certs(true).user_agent("fixture").build();
    let client = Blocking::builder().danger_accept_invalid_certs(true);
    client.timeout(duration()).danger_accept_invalid_hostnames(true).build();
    reqwest::Client::builder().tls_danger_accept_invalid_certs(true)
        .tls_danger_accept_invalid_certs(false).build();
    reqwest::Client::builder().tls_danger_accept_invalid_certs(true)
        .tls_danger_accept_invalid_certs(flag).build();
    let mut connector = TlsConnector::builder();
    connector.danger_accept_invalid_certs(true);
    connector.danger_accept_invalid_certs(false);
    connector.danger_accept_invalid_hostnames(true);
    connector.build();
    let mut escaped = TlsConnector::builder();
    escaped.danger_accept_invalid_certs(true);
    reconfigure(&mut escaped);
    escaped.build();
    external::Client::builder().danger_accept_invalid_certs(true).build();
    reqwest::Client::builder().danger_accept_invalid_certs(true).use_preconfigured_tls(config()).build();
}
mod shadow {
    mod reqwest {}
    fn build() { reqwest::Client::builder().danger_accept_invalid_certs(true).build(); }
}
"#;
        let findings = analyze_file("test.rs", source);
        let mut tls: Vec<_> = findings
            .iter()
            .filter_map(|finding| {
                let id = finding.source.rule_id.as_deref()?;
                id.starts_with("rust-tls-invalid-")
                    .then_some((finding.location.range.start_line, id))
            })
            .collect();
        tls.sort_unstable();
        assert_eq!(
            tls,
            [
                (6, "rust-tls-invalid-certificates"),
                (7, "rust-tls-invalid-certificates"),
                (8, "rust-tls-invalid-hostnames"),
                (16, "rust-tls-invalid-hostnames"),
            ],
            "{findings:#?}"
        );
    }
}
