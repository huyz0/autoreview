use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::web::node_calls::{property, string_literal, NodeCalls};
use super::{children, descendants, location, Rule};

const VERIFY_EXPIRATION: Rule = Rule {
    id: "node-jwt-ignores-expiration",
    category: "security",
    subcategories: &["auth.token-validation"],
    title: "JWT verifier accepts expired tokens",
    message: "This jsonwebtoken verifier skips the expiration check. Keep expiration checking enabled when accepting tokens for authentication.",
    reference: "https://github.com/auth0/node-jsonwebtoken#jwtverifytoken-secretorpublickey-options-callback",
};
const VERIFY_NOT_BEFORE: Rule = Rule {
    id: "node-jwt-ignores-not-before",
    title: "JWT activation time check disabled",
    message: "This jsonwebtoken verifier skips the not-before check. Enforce the token's activation time before using its claims for authentication.",
    ..VERIFY_EXPIRATION
};
const COOKIE_SECURE: Rule = Rule {
    id: "express-session-cookie-without-secure",
    category: "security",
    subcategories: &["auth.session-cookie"],
    title: "Session cookie lacks HTTPS-only protection",
    message: "This Express session middleware explicitly disables the Secure flag. Enable it for HTTPS deployments and configure trusted proxy behavior where needed.",
    reference: "https://expressjs.com/en/resources/middleware/session/#cookiesecure",
};
const COOKIE_HTTP_ONLY: Rule = Rule {
    id: "express-session-cookie-without-http-only",
    title: "Client scripts can read the session cookie",
    message: "This Express session middleware explicitly disables the HttpOnly flag. Keep HttpOnly enabled so browser scripts cannot directly read the session cookie.",
    reference: "https://expressjs.com/en/resources/middleware/session/#cookiehttponly",
    ..COOKIE_SECURE
};
const ECB: Rule = Rule {
    id: "node-aes-ecb-encryption",
    category: "security",
    subcategories: &["crypto.cipher-mode"],
    title: "Node encryption uses AES in ECB mode",
    message: "ECB exposes patterns in plaintext. Use an authenticated encryption mode such as AES-GCM for new encryption.",
    reference: "https://cheatsheetseries.owasp.org/cheatsheets/Cryptographic_Storage_Cheat_Sheet.html#cipher-modes",
};

pub(super) const RULES: &[&Rule] = &[
    &VERIFY_EXPIRATION,
    &VERIFY_NOT_BEFORE,
    &COOKIE_SECURE,
    &COOKIE_HTTP_ONLY,
    &ECB,
];
const MODULES: &[&str] = &[
    "jsonwebtoken",
    "express-session",
    "cookie-session",
    "node:crypto",
    "crypto",
];

pub(super) fn inspect(path: &str, source: &str, root: Node<'_>) -> Vec<AgentFinding> {
    if !MODULES.iter().any(|module| source.contains(module)) {
        return Vec::new();
    }
    let calls = NodeCalls::new(root, source);
    let mut findings = Vec::new();
    for call in descendants(root).filter(|node| node.kind() == "call_expression") {
        let Some(api) = calls.resolve(call, MODULES) else {
            continue;
        };
        match api {
            ("jsonwebtoken", "verify") => {
                let Some(options) =
                    argument(call, 2).and_then(|arg| calls.options_object(arg, call))
                else {
                    continue;
                };
                report_flags(
                    path,
                    source,
                    options,
                    call,
                    "true",
                    &[
                        ("ignoreExpiration", &VERIFY_EXPIRATION, "CWE-613"),
                        ("ignoreNotBefore", &VERIFY_NOT_BEFORE, "CWE-287"),
                    ],
                    &mut findings,
                );
            }
            ("express-session" | "cookie-session", "default") => {
                let Some(cookie) = session_options(&calls, call, api.0, source) else {
                    continue;
                };
                report_flags(
                    path,
                    source,
                    cookie,
                    call,
                    "false",
                    &[
                        ("secure", &COOKIE_SECURE, "CWE-614"),
                        ("httpOnly", &COOKIE_HTTP_ONLY, "CWE-1004"),
                    ],
                    &mut findings,
                );
            }
            ("node:crypto" | "crypto", "createCipheriv") => {
                if let Some(algorithm) = argument(call, 0).filter(|node| is_aes_ecb(*node, source))
                {
                    findings.push(finding(&ECB, path, algorithm, call, source, "CWE-327"));
                }
            }
            _ => {}
        }
    }
    findings
}

fn session_options<'t>(
    calls: &NodeCalls<'t, '_>,
    call: Node<'t>,
    module: &str,
    source: &str,
) -> Option<Node<'t>> {
    let options = calls.options_object(argument(call, 0)?, call)?;
    match module {
        "express-session" => Some(property(options, "cookie", source)?.1),
        "cookie-session" => Some(options),
        _ => None,
    }
}

fn argument(call: Node<'_>, index: usize) -> Option<Node<'_>> {
    children(call.child_by_field_name("arguments")?)
        .filter(|node| node.kind() != "comment")
        .take_while(|node| node.kind() != "spread_element")
        .nth(index)
}

fn is_aes_ecb(node: Node<'_>, source: &str) -> bool {
    string_literal(node, source).is_some_and(|name| {
        ["aes-128-ecb", "aes-192-ecb", "aes-256-ecb"]
            .iter()
            .any(|cipher| name.eq_ignore_ascii_case(cipher))
    })
}

fn report_flags(
    path: &str,
    source: &str,
    object: Node<'_>,
    call: Node<'_>,
    expected: &str,
    flags: &[(&str, &Rule, &str)],
    findings: &mut Vec<AgentFinding>,
) {
    findings.extend(flags.iter().filter_map(|(name, rule, cwe)| {
        let (setting, value) = property(object, name, source)?;
        (value.kind() == expected).then(|| finding(rule, path, setting, call, source, cwe))
    }));
}

fn finding(
    rule: &Rule,
    path: &str,
    setting: Node<'_>,
    call: Node<'_>,
    source: &str,
    cwe: &str,
) -> AgentFinding {
    let mut finding = rule.finding(
        location(path, setting, source),
        vec![location(path, call, source)],
    );
    finding
        .meta
        .get_or_insert_with(Default::default)
        .insert("cwe".into(), serde_json::json!([cwe]));
    finding
}
