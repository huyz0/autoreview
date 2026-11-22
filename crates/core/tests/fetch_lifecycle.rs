use autoreview_core::run_framework_checks;
use autoreview_test_support::init_repo;

const BODY: &str = "typescript-fetch-response-body-read-twice";
const STATUS: &str = "typescript-fetch-json-without-status-check";

#[test]
fn response_identity_survives_aliases_branches_and_reassignment() {
    let source = r#"
async function doubleRead(flag: boolean) {
    let response = await fetch('/first');
    const alias = response;
    if (flag) await alias.text();
    response = await fetch('/second');
    await response.text();
    await alias.arrayBuffer();
}
async function exclusive(flag: boolean) {
    const response = await fetch('/');
    if (flag) {
        await response.text();
    } else {
        await response.json();
    }
}
async function oppositeGuards(flag: boolean) {
    const response = await fetch('/');
    if (flag) await response.text();
    if (!flag) await response.json();
}
async function singleStatementElse(flag: boolean) {
    const response = await fetch('/');
    if (flag) return;
    else await response.blob();
    await response.text();
}
async function unreachable() {
    const response = await fetch('/');
    await response.text();
    if (false) await response.json();
}
async function cloneBeforeReading() {
    const response = await fetch('/');
    const clone = response.clone();
    await response.blob();
    await clone.json();
}
async function guardedConsumption() {
    const response = await fetch('/');
    await response.text();
    if (!response.bodyUsed) await response.json();
}
async function lexicalShadowing() {
    const response = await fetch('/');
    {
        const response = await fetch('/other');
        await response.arrayBuffer();
    }
    await response.text();
}
async function overwritten() {
    let response = await fetch('/');
    await response.text();
    response = new Response();
    await response.text();
}
async function reallocatedInLoop(urls: string[]) {
    for (const url of urls) {
        const response = await fetch(url);
        await response.text();
    }
}
"#;
    let findings = review("responses.ts", source);
    assert_eq!(findings.len(), 2, "{findings:#?}");
    let finding = &findings[0];
    assert_eq!(finding.source.rule_id.as_deref(), Some(BODY));
    assert_eq!(finding.location.snippet, "alias.arrayBuffer()");
    assert_eq!(
        finding.related_locations.as_ref().unwrap()[0].snippet,
        "alias.text()"
    );
    assert_eq!(findings[1].location.snippet, "response.text()");
    assert_eq!(
        findings[1].related_locations.as_ref().unwrap()[0].snippet,
        "response.blob()"
    );
}

#[test]
fn response_status_must_be_handled_on_the_path_that_returns_json() {
    let source = r#"
import { fetch as request } from 'undici';

async function guarded() {
    const r = await request('/');
    const alias = r;
    if (!alias.ok) throw new Error('request failed');
    const payload = await r.json();
    return payload;
}
async function numeric() {
    const r = await request('/');
    if (r.status >= 400) return null;
    return r.json();
}
async function selectedStatus() {
    const r = await request('/');
    if (r.status === 200) return r.json();
    return null;
}
async function explicitError() {
    const r = await request('/');
    if (!r.ok) return r.json();
    return { complete: true };
}
async function checkDifferentResponse() {
    const r = await request('/');
    const unrelated = await request('/other');
    if (!unrelated.ok) return null;
    return await r.json();
}
async function partialGuard(shouldCheck: boolean) {
    const r = await request('/');
    if (shouldCheck && !r.ok) return null;
    const payload = await r.json();
    return payload;
}
async function ineffective() {
    const r = await request('/');
    if (!r.ok) console.debug(r.status);
    return r.json();
}
async function forwarded() {
    return request('/');
}
function promised() {
    return request('/').then(r => r.json());
}
async function shadowed(request: CustomClient) {
    const response = await request('/');
    return response.json();
}
"#;
    let findings = review("status.tsx", source);
    assert_eq!(findings.len(), 4, "{findings:#?}");
    for finding in &findings {
        assert_eq!(finding.source.rule_id.as_deref(), Some(STATUS));
        assert_eq!(finding.location.snippet, "r.json()");
        assert_eq!(
            finding.related_locations.as_ref().unwrap()[0].snippet,
            "request('/')"
        );
    }
}

#[test]
fn original_javascript_examples_keep_their_existing_ids() {
    let body = include_str!("fixtures/rules/javascript-fetch-response-body-read-twice/positive.js");
    let status =
        include_str!("fixtures/rules/javascript-fetch-json-without-status-check/positive.js");
    let negative =
        include_str!("fixtures/rules/javascript-fetch-json-without-status-check/negative.js");
    let negative_body =
        include_str!("fixtures/rules/javascript-fetch-response-body-read-twice/negative.js");
    for (source, expected) in [
        (body, "javascript-fetch-response-body-read-twice"),
        (status, "javascript-fetch-json-without-status-check"),
    ] {
        let findings = review("index.js", source);
        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert_eq!(findings[0].source.rule_id.as_deref(), Some(expected));
    }
    for (source, rule) in [
        (negative, "javascript-fetch-json-without-status-check"),
        (negative_body, "javascript-fetch-response-body-read-twice"),
    ] {
        let findings = review("index.js", source);
        assert!(
            findings
                .iter()
                .all(|finding| finding.source.rule_id.as_deref() != Some(rule)),
            "{findings:#?}"
        );
    }
}

#[test]
fn fetch_casts_require_a_known_response_and_concrete_type() {
    let cast_id = "typescript-json-cast-without-validation";
    let positive =
        include_str!("fixtures/rules/typescript-json-cast-without-validation/positive.ts");
    let negative =
        include_str!("fixtures/rules/typescript-json-cast-without-validation/negative.ts");
    for path in ["customer.ts", "customer.tsx"] {
        let findings = review(path, positive);
        let cast = findings
            .iter()
            .find(|finding| finding.source.rule_id.as_deref() == Some(cast_id));
        assert_eq!(
            cast.map(|finding| finding.location.snippet.as_str()),
            Some("(await response.json()) as Customer")
        );
        let findings = review(path, negative);
        assert!(findings
            .iter()
            .all(|finding| finding.source.rule_id.as_deref() != Some(cast_id)));
    }
    let source = r#"
import { fetch as request } from 'undici';
async function aliased() {
    const r = await request('/');
    if (!r.ok) return null;
    const alias = r;
    const customer = (await alias.json()) as Customer;
    return customer;
}
async function arbitrary(fetch: Client) {
    const r = await fetch('/');
    const customer = (await r.json()) as Customer;
    return customer;
}
async function unknown() {
    const r = await request('/');
    const body = (await r.json()) as unknown;
    return schema.parse(body);
}
"#;
    let findings = review("alias.ts", source);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].source.rule_id.as_deref(), Some(cast_id));
}

fn review(path: &str, source: &str) -> Vec<autoreview_schema::AgentFinding> {
    let repo = init_repo(&[(path, source)]);
    run_framework_checks(repo.path(), &[path.into()])
}
