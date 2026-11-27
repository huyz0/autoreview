use autoreview_core::run_framework_checks;
use std::fs;
use tempfile::TempDir;

fn scan(sources: &[(&str, &str)], changed: &[&str]) -> Vec<autoreview_schema::AgentFinding> {
    let repo = TempDir::new().unwrap();
    for (path, source) in sources {
        let target = repo.path().join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, source).unwrap();
    }
    run_framework_checks(
        repo.path(),
        &changed
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>(),
    )
}

#[test]
fn node_tls_reports_only_direct_literal_assignments_to_the_ambient_process() {
    let js = r#"process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';
process.env.NODE_TLS_REJECT_UNAUTHORIZED = '1';
process.env.NODE_TLS_REJECT_UNAUTHORIZED = configuredValue;
process.env.NODE_TLS_REJECT_UNAUTHORIZED ||= '0';
const settings = { NODE_TLS_REJECT_UNAUTHORIZED: '0' };
const current = process.env.NODE_TLS_REJECT_UNAUTHORIZED;
function update(process) { process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0'; }
{ const process = fixture; process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0'; }
const updateAgain = function process() { process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0'; };
"#;
    let ts = r#"process["env"]["NODE_TLS_REJECT_UNAUTHORIZED"] = "0";
process.env.OTHER_SETTING = '0';
"#;
    let tsx = "const view = <main />;\nprocess.env['NODE_TLS_REJECT_UNAUTHORIZED'] = '0';";
    let imported = "import { mock as process } from './mock';\nprocess.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';";
    let found = scan(
        &[("settings.js", js), ("settings.ts", ts), ("view.tsx", tsx), ("mocked.ts", imported)],
        &["settings.js", "settings.ts", "view.tsx", "mocked.ts"],
    );
    let locations: Vec<_> = found.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some("node-tls-verification-disabled-by-environment"))
        .map(|finding| (finding.location.path.as_str(), finding.location.range.start_line))
        .collect();
    assert_eq!(locations, [("settings.js", 1), ("settings.ts", 1), ("view.tsx", 2)]);
}

#[test]
fn node_tls_client_options_are_linked_to_the_resolved_operation_and_final_value() {
    let js = r#"const https = require('https');
const options = { "rejectUnauthorized": false };
https.get('https://service.test', options);
require('node:tls').connect({ rejectUnauthorized: false });
new https.Agent({ rejectUnauthorized: true });
new https.Agent({ rejectUnauthorized: false, ...settings });
new https.Agent({ rejectUnauthorized: false, rejectUnauthorized: true });
new https.Agent({ ...settings, rejectUnauthorized: false });
const unrelated = { rejectUnauthorized: false };
other.Agent({ rejectUnauthorized: false });
const changed = { rejectUnauthorized: false };
changed.rejectUnauthorized = true;
new https.Agent(changed);
function fixture(https) { new https.Agent({ rejectUnauthorized: false }); }
"#;
    let ts = r#"import { connect as dial } from 'node:tls';
import * as web from 'node:https';
import type { Agent as TypeOnly } from 'node:https';
dial({ rejectUnauthorized: false });
web.request({ 'rejectUnauthorized': false });
web.createServer({ rejectUnauthorized: false });
new TypeOnly({ rejectUnauthorized: false });
dial(443, 'service.test', { rejectUnauthorized: false });
dial(443, { rejectUnauthorized: false, ['rejectUnauthorized']: true });
for (const web of clients) { web.request({ rejectUnauthorized: false }); }
function useLocalClient() {
    for (var web of clients) { use(web); }
    web.request({ rejectUnauthorized: false });
}
"#;
    let tsx = "import https from 'https';\nconst view = <main />;\nnew https.Agent({ rejectUnauthorized: false });";
    let found = scan(&[("client.js", js), ("client.ts", ts), ("view.tsx", tsx)], &["client.js", "client.ts", "view.tsx"]);
    let locations: Vec<_> = found.iter()
        .filter(|finding| matches!(finding.source.rule_id.as_deref(),
            Some("javascript-tls-verification-disabled" | "typescript-tls-verification-disabled")))
        .map(|finding| (finding.location.path.as_str(), finding.location.range.start_line))
        .collect();
    assert_eq!(locations, [("client.js", 2), ("client.js", 4), ("client.js", 8), ("client.ts", 4), ("client.ts", 5), ("client.ts", 8), ("view.tsx", 3)], "{found:#?}");
    assert!(found.iter().any(|finding| finding.related_locations.as_ref().is_some_and(|sites| {
        sites.iter().any(|site| site.path == "client.js" && site.range.start_line == 3)
    })));
    assert!(found.iter().filter(|finding| matches!(finding.source.rule_id.as_deref(),
        Some("javascript-tls-verification-disabled" | "typescript-tls-verification-disabled")))
        .all(|finding| finding.severity == autoreview_schema::Severity::High));
}

#[test]
fn go_tls_assignment_requires_a_config_from_crypto_tls() {
    let go = r#"package fixture
import secure "crypto/tls"
type config struct { InsecureSkipVerify bool }
func configure(incoming *secure.Config) {
    cfg := &secure.Config{}
    cfg.InsecureSkipVerify = true
    incoming.InsecureSkipVerify = true
    var typed secure.Config
    typed.InsecureSkipVerify = true
    cfg.InsecureSkipVerify = false
    local := config{}
    local.InsecureSkipVerify = true
    {
        cfg := config{}
        cfg.InsecureSkipVerify = true
    }
}
func shadowLiteral(incoming *secure.Config) {
    true := false
    incoming.InsecureSkipVerify = true
}
"#;
    let unrelated = r#"package unrelated
import secure "example.test/tls"
func configure(cfg *secure.Config) { cfg.InsecureSkipVerify = true }
"#;
    let shadowed_peer = "package shadowed\nconst true = false\n";
    let shadowed_client = "package shadowed\nimport \"crypto/tls\"\nfunc configure(cfg *tls.Config) { cfg.InsecureSkipVerify = true }\n";
    let found = scan(
        &[("client.go", go), ("different/client.go", unrelated), ("shadowed/override.go", shadowed_peer), ("shadowed/client.go", shadowed_client)],
        &["client.go", "different/client.go", "shadowed/client.go"],
    );
    let lines: Vec<_> = found.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some("go-tls-verification-disabled-by-assignment"))
        .map(|finding| (finding.location.path.as_str(), finding.location.range.start_line))
        .collect();
    assert_eq!(lines, [("client.go", 6), ("client.go", 7), ("client.go", 9)], "{found:#?}");
}

#[test]
fn node_security_flags_require_static_module_and_final_literal_options() {
    let js = r#"const jwt = require('jsonwebtoken');
const session = require('express-session');
const { createCipheriv: encrypt, createDecipheriv } = require('node:crypto');
jwt.verify(token, key, { ignoreExpiration: true, 'ignoreNotBefore': true });
jwt.verify(token, key, { ignoreExpiration: true, ...dynamicOptions });
jwt.verify(token, key, { ignoreExpiration: true, ignoreExpiration: false });
jwt.verify(...values, key, { ignoreExpiration: true });
jwt.decode(token, { ignoreExpiration: true });
session({ cookie: { secure: false, httpOnly: false } });
session({ cookie: { secure: false }, cookie: { secure: true } });
session({ cookie: { secure: 'auto', httpOnly: true } });
encrypt('AES-128-ECB', key, null);
createDecipheriv('aes-128-ecb', key, null);
encrypt('aes-128-gcm', key, iv);
encrypt(algorithm, key, iv);
function fixture(jwt, encrypt) {
    jwt.verify(token, key, { ignoreExpiration: true });
    encrypt('aes-256-ecb', key, null);
}
"#;
    let ts = r#"import sessions from 'cookie-session';
import { verify as verifyToken } from 'jsonwebtoken';
import { createCipheriv } from 'node:crypto';
import { verify as custom } from './fixture';
const options = { 'ignoreNotBefore': true };
verifyToken(token, key, options);
sessions({ 'httpOnly': false, secure: true });
require('express-session')({ cookie: { secure: false } });
custom(token, key, { ignoreExpiration: true });
for (const createCipheriv of factories) { createCipheriv('aes-256-ecb', key, null); }
const view = <main />;
createCipheriv('aes-256-ecb', key, null);
"#;
    let sources = [("settings.js", js), ("settings.tsx", ts)];
    let findings = scan(&sources, &["settings.js", "settings.tsx"]);
    let mut actual: Vec<_> = findings.iter().filter(|finding| finding.category == "security")
        .map(|finding| {
            let id = finding.source.rule_id.as_deref().unwrap();
            let site = &finding.location;
            (site.path.as_str(), site.range.start_line, id)
        }).collect();
    actual.sort_unstable();
    assert_eq!(actual, [
        ("settings.js", 4, "node-jwt-ignores-expiration"),
        ("settings.js", 4, "node-jwt-ignores-not-before"),
        ("settings.js", 9, "express-session-cookie-without-http-only"),
        ("settings.js", 9, "express-session-cookie-without-secure"),
        ("settings.js", 12, "node-aes-ecb-encryption"),
        ("settings.tsx", 5, "node-jwt-ignores-not-before"),
        ("settings.tsx", 7, "express-session-cookie-without-http-only"),
        ("settings.tsx", 8, "express-session-cookie-without-secure"),
        ("settings.tsx", 12, "node-aes-ecb-encryption"),
    ], "{findings:#?}");
}

#[test]
fn go_jwt_v5_reports_direct_parser_options_only() {
    let src = r#"package fixture
import tokens "github.com/golang-jwt/jwt/v5"
func parse() {
    tokens.NewParser(tokens.WithoutClaimsValidation())
    tokens.Parse(value, key, tokens.WithoutClaimsValidation())
    tokens.ParseWithClaims(value, claims, key, tokens.WithoutClaimsValidation())
    tokens.NewParser(tokens.WithExpirationRequired())
    takeOption(tokens.WithoutClaimsValidation())
}
func unrelated(tokens TestParser) { tokens.NewParser(tokens.WithoutClaimsValidation()) }
var parser = tokens.NewParser(tokens.WithoutClaimsValidation())
"#;
    let old = r#"package fixture
import tokens "github.com/golang-jwt/jwt/v4"
func parseOld() { tokens.NewParser(tokens.WithoutClaimsValidation()) }
"#;
    let findings = scan(&[("client.go", src), ("old/client.go", old)], &["client.go", "old/client.go"]);
    let actual: Vec<_> = findings.iter().filter(|finding| {
        finding.source.rule_id.as_deref() == Some("go-jwt-v5-claims-validation-disabled")
    }).map(|finding| (finding.location.path.as_str(), finding.location.range.start_line)).collect();
    assert_eq!(actual, [("client.go", 4), ("client.go", 5), ("client.go", 6), ("client.go", 11)], "{findings:#?}");
}

#[test]
fn constructor_dispatch_reports_resolved_overridable_calls() {
    let source = r#"class Base {
    Base(String value) {
        if (value != null) { this.configure(value); }
        cache();
        metric();
        local();
        Runnable later = () -> configure(value);
        Runnable listener = new Runnable() {
            public void run() { configure(value); }
        };
        new Base(null).configure(value);
        overload(value);
    }

    protected void configure(String value) {}
    private void local() {}
    final void cache() {}
    static void metric() {}
    void overload(String value) {}
    void overload(Object value) {}
}
final class Closed {
    Closed() { configure(); }
    void configure() {}
}
class Parent {
    final void resolve(String key) {}
}
class Child extends Parent {
    Child() { resolve("key"); }
    void resolve(Object key) {}
}
interface Provider {
    default void resolve(String key) {}
}
class Adapter implements Provider {
    Adapter() { resolve("key"); }
    void resolve(Object key) {}
}
"#;
    let found = scan(&[("Base.java", source)], &["Base.java"]);
    let calls: Vec<_> = found
        .iter()
        .map(|item| item.location.snippet.as_str())
        .collect();
    assert_eq!(
        calls,
        [
            "this.configure(value)",
            "overload(value)",
            "resolve(\"key\")"
        ],
        "{found:#?}"
    );
    assert_eq!(
        found[0].source.rule_id.as_deref(),
        Some("java-constructor-calls-overridable-method")
    );
    assert_eq!(found[0].location.snippet, "this.configure(value)");
    assert_eq!(
        found[0].related_locations.as_ref().unwrap()[0].snippet,
        "protected void configure(String value) {}"
    );
}

#[test]
fn outbound_http_calls_in_handlers_keep_the_parent_context() {
    let source = r#"package example
import ctx "context"
import web "net/http"
func proxy(w web.ResponseWriter, req *web.Request) {
    _, _ = web.NewRequest("GET", "https://example.test", nil)
    _, _ = web.NewRequestWithContext(ctx.Background(), "GET", "https://example.test", nil)
    _, _ = web.NewRequestWithContext(req.Context(), "GET", "https://example.test", nil)
    _, _ = web.NewRequestWithContext(ctx.WithoutCancel(req.Context()), "GET", "https://example.test", nil)
    go func() { _, _ = web.NewRequest("GET", "https://example.test", nil) }()
}
func helper() {
    _, _ = web.NewRequest("GET", "https://example.test", nil)
}
"#;
    let found = scan(&[("proxy.go", source)], &["proxy.go"]);
    assert_eq!(found.len(), 2, "{found:#?}");
    for finding in found {
        assert_eq!(
            finding.source.rule_id.as_deref(),
            Some("go-http-handler-discards-request-context")
        );
        assert_eq!(
            finding.related_locations.as_ref().unwrap()[0].snippet,
            "req"
        );
    }
}

#[test]
fn spring_reports_explicitly_unpaged_get_endpoints_with_known_repositories() {
    let source = r#"import org.springframework.data.domain.Pageable;
import org.springframework.data.repository.PagingAndSortingRepository;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController class Endpoint {
    PagingAndSortingRepository<String, Long> rows;
    @GetMapping("/all") Object all() {
        return this.rows.findAll(Pageable.unpaged());
    }
    @GetMapping("/page") Object page(Pageable page) {
        return rows.findAll(page);
    }
    @GetMapping("/inventory") Object unrelated(Inventory rows) {
        return rows.findAll(Pageable.unpaged());
    }
    Object batch() {
        return rows.findAll(Pageable.unpaged());
    }
}
"#;
    let found = scan(&[("Endpoint.java", source)], &["Endpoint.java"]);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(
        found[0].source.rule_id.as_deref(),
        Some("java-spring-unpaged-repository-endpoint")
    );
    assert_eq!(
        found[0].location.snippet,
        "this.rows.findAll(Pageable.unpaged())"
    );
}

#[test]
fn open_telemetry_span_ownership_tracks_return_paths() {
    let source = r#"package demo
import "context"
import "go.opentelemetry.io/otel"
import oteltrace "go.opentelemetry.io/otel/trace"
func leak(ctx context.Context, tracer oteltrace.Tracer, early bool) {
    _, span := tracer.Start(ctx, "load")
    span.AddEvent("started")
    if early { return }
    span.End()
}
func close(ctx context.Context) {
    _, span := otel.Tracer("demo").Start(ctx, "load")
    defer span.End()
}
func forward(ctx context.Context, tracer oteltrace.Tracer) oteltrace.Span {
    _, span := tracer.Start(ctx, "load")
    return span
}
func other(ctx context.Context, unrelated OtherTracer) {
    _, span := unrelated.Start(ctx, "load")
    span.AddEvent("started")
}
func discard(parent context.Context) context.Context {
    ctx, _ := otel.Tracer("demo").Start(parent, "load")
    return ctx
}
"#;
    let found = scan(&[("trace.go", source)], &["trace.go"]);
    assert_eq!(found.len(), 2, "{found:#?}");
    assert_eq!(
        found[0].source.rule_id.as_deref(),
        Some("go-otel-span-not-ended")
    );
    assert_eq!(found[0].location.range.start_line, 6);
    assert_eq!(
        found[0].related_locations.as_ref().unwrap()[0]
            .range
            .start_line,
        8
    );
}

#[test]
fn otel_reports_a_fresh_context_only_when_a_parent_is_available() {
    let repo = tempfile::tempdir().unwrap();
    let path = "trace.go";
    std::fs::write(repo.path().join(path), r#"
package sample
import (
    ctxpkg "context"
    "go.opentelemetry.io/otel/trace"
)
func incoming(parent ctxpkg.Context, tracer trace.Tracer) {
    _, span := tracer.Start(ctxpkg.Background(), "child")
    defer span.End()
}
func preserved(parent ctxpkg.Context, tracer trace.Tracer) {
    _, span := tracer.Start(parent, "child")
    defer span.End()
}
func root(tracer trace.Tracer) {
    _, span := tracer.Start(ctxpkg.Background(), "root")
    defer span.End()
}
"#).unwrap();
    let findings = autoreview_core::run_framework_checks(repo.path(), &[path.into()]);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].source.rule_id.as_deref(), Some("go-otel-span-discards-parent-context"));
    assert_eq!(findings[0].location.snippet, "ctxpkg.Background()");
    assert_eq!(findings[0].related_locations.as_ref().unwrap()[0].snippet, "parent");
}

#[test]
fn open_telemetry_scopes_must_have_an_owner() {
    let source = r#"import io.opentelemetry.api.trace.Span;
import io.opentelemetry.context.Context;
class Example {
    void leak(Span span) {
        span.makeCurrent();
    }
    void leakCurrent() {
        Context.current().makeCurrent();
    }
    void scoped(Span span) {
        try (var scope = span.makeCurrent()) { work(); }
    }
    void unrelated(Other span) {
        span.makeCurrent();
    }
    void local() {
        Span span = Span.current();
        span.makeCurrent();
    }
}
"#;
    let found = scan(&[("Example.java", source)], &["Example.java"]);
    let lines: Vec<_> = found
        .iter()
        .map(|finding| finding.location.range.start_line)
        .collect();
    assert_eq!(lines, [5, 8, 18], "{found:#?}");
}

#[test]
fn go_cancellation_follows_branch_exits_and_package_local_helpers() {
    let callers = r#"package demo
import "context"
func branch(parent context.Context, stop bool) {
    _, cancel := newContext(parent)
    if stop { return }
    cleanup(cancel)
}
func deferred(parent context.Context) {
    _, cancel := newContext(parent)
    defer cleanup(cancel)
}
func returned(parent context.Context) (context.Context, context.CancelFunc) {
    ctx, cancel := newContext(parent)
    return ctx, cancel
}
func partial(parent context.Context, stop bool) {
    _, cancel := newContext(parent)
    conditionalCleanup(cancel, stop)
}
func safeBranches(parent context.Context, stop bool) {
    _, cancel := context.WithCancel(parent)
    if stop { cancel(); return } else { defer cancel() }
}
func externalTransfer(parent context.Context) {
    _, cancel := newContext(parent)
    acceptOwnership(cancel)
}
"#;
    let helpers = r#"package demo
import stdctx "context"
func newContext(parent stdctx.Context) (stdctx.Context, stdctx.CancelFunc) {
    return stdctx.WithCancel(parent)
}
func cleanup(cancel stdctx.CancelFunc) { finish(cancel) }
func conditionalCleanup(cancel stdctx.CancelFunc, stop bool) {
    conditionalFinish(cancel, stop)
}
"#;
    let leaf = r#"package demo
import "context"
func finish(cancel context.CancelFunc) { defer cancel() }
func conditionalFinish(cancel context.CancelFunc, stop bool) {
    if stop { cancel() }
}
"#;
    let files = [
        ("service/callers.go", callers),
        ("service/helpers.go", helpers),
        ("service/leaf.go", leaf),
    ];
    let found = scan(&files, &["service/callers.go"]);
    let lines: Vec<_> = found
        .iter()
        .map(|finding| finding.location.range.start_line)
        .collect();
    assert_eq!(lines, [4, 17], "{found:#?}");
    assert!(
        found
            .iter()
            .all(|finding| finding.source.rule_id.as_deref()
                == Some("go-context-cancel-on-all-paths"))
    );
    let helper_change = scan(&files, &["service/leaf.go"]);
    assert!(helper_change
        .iter()
        .any(|finding| finding.location.path == "service/callers.go"
            && finding
                .related_locations
                .as_ref()
                .unwrap()
                .iter()
                .any(|site| site.path == "service/leaf.go")));
}

#[test]
fn spring_reports_only_resolved_same_instance_calls_in_ordinary_proxy_usage() {
    let java = r#"import org.springframework.transaction.annotation.Transactional;
class Service {
    public void orchestrate(Service other) {
        this.save();
        save();
        other.save();
        overloaded(1);
        direct();
    }
    @Transactional public void save() {}
    @Transactional public void overloaded(int value) {}
    public void overloaded(String value) {}
    @Transactional private void direct() {}
    @Transactional public void existingTransaction() { save(); }
}
"#;
    let found = scan(&[("Service.java", java)], &["Service.java"]);
    let lines: Vec<_> = found
        .iter()
        .map(|finding| finding.location.range.start_line)
        .collect();
    assert_eq!(lines, [4, 5, 7], "{found:#?}");
    let woven = format!(
        "import org.springframework.transaction.annotation.EnableTransactionManagement;\n{java}\n@EnableTransactionManagement(mode = AdviceMode.ASPECTJ) class Config {{}}"
    );
    assert!(scan(&[("Service.java", &woven)], &["Service.java"]).is_empty());
}

fn cross_file_evidence(findings: &[autoreview_schema::AgentFinding]) -> Vec<(&str, &str)> {
    findings
        .iter()
        .map(|finding| {
            let target = &finding.related_locations.as_ref().unwrap()[0];
            (finding.location.snippet.as_str(), target.path.as_str())
        })
        .collect()
}

#[test]
fn constructor_resolves_inherited_protected_methods_and_interface_defaults() {
    let base = r#"package parents;
public class Parent {
    protected void prepare(String task) {}
    protected final void prepare(Object task) {}
    protected final void fixed() {}
    void packageOnly() {}
    private void hidden() {}
}
"#;
    let contract = r#"package parents;
public interface Contract {
    default void check() {}
}
"#;
    let child = r#"package app;
import parents.Parent;
import parents.Contract;
public class Child extends Parent implements Contract {
    Child(String task) {
        prepare(task);
        this.prepare((Object) task);
        super.prepare(task);
        fixed();
        check();
        checkDeferred(() -> check());
    }
    private void checkDeferred(Runnable later) {}
}
"#;
    let found = scan(
        &[
            ("parents/Parent.java", base),
            ("parents/Contract.java", contract),
            ("app/Child.java", child),
        ],
        &["app/Child.java"],
    );
    assert_eq!(
        cross_file_evidence(&found),
        [
            ("prepare(task)", "parents/Parent.java"),
            ("check()", "parents/Contract.java")
        ],
        "{found:#?}"
    );
}

#[test]
fn inherited_transaction_resolution_honors_dispatch_and_override_metadata() {
    let parent = r#"package model;
import org.springframework.transaction.annotation.Transactional;
public class BaseService {
    @Transactional public void save(String name) {}
    public void save(Object name) {}
    @Transactional public void inherited() {}
    @Transactional public void overridden() {}
}
"#;
    let api = r#"package model;
import org.springframework.transaction.annotation.Transactional;
public interface TaskApi {
    @Transactional default void refresh() {}
}
"#;
    let worker = r#"package application;
import model.BaseService;
import model.TaskApi;
import org.springframework.transaction.annotation.Transactional;
class Worker extends BaseService implements TaskApi {
    public void run(Worker proxy, String value, Object payload) {
        this.save(value);
        super.save(value);
        save(payload);
        save(null);
        proxy.save(value);
        inherited();
        overridden();
        refresh();
    }
    @Override public void overridden() {}
    @Transactional public void alreadyTransactional() { inherited(); }
}
"#;
    let found = scan(
        &[
            ("model/BaseService.java", parent),
            ("model/TaskApi.java", api),
            ("application/Worker.java", worker),
        ],
        &["application/Worker.java"],
    );
    assert_eq!(
        cross_file_evidence(&found),
        [
            ("this.save(value)", "model/BaseService.java"),
            ("super.save(value)", "model/BaseService.java"),
            ("inherited()", "model/BaseService.java"),
            ("overridden()", "model/BaseService.java"),
            ("refresh()", "model/TaskApi.java"),
        ],
        "{found:#?}"
    );
}

#[test]
fn inherited_lookup_ignores_inaccessible_and_unresolved_candidates() {
    let base = r#"package common;
import org.springframework.transaction.annotation.Transactional;
public class Base {
    @Transactional void checkpoint() {}
    @Transactional private void reset() {}
    @Transactional protected void write(Object value) {}
}
"#;
    let bridge = r#"package unrelated;
public class Bridge extends common.Base {}
"#;
    let child = r#"package common;
import org.springframework.transaction.annotation.Transactional;
class Child extends unrelated.Bridge {
    Child() { reset(); }
    void run() {
        checkpoint();
        write(value());
    }
    private void reset() {}
    private void checkpoint() {}
    protected void write(String text) {}
    String value() { return ""; }
}
class UnknownHierarchy extends vendor.Parent {
    void run() { write("text"); }
    @Transactional void write(Object object) {}
}
"#;
    let found = scan(
        &[
            ("Base.java", base),
            ("Bridge.java", bridge),
            ("Child.java", child),
        ],
        &["Child.java"],
    );
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn hibernate_correlates_typed_results_with_lazy_mapping_and_materialization() {
    let model = r#"package model;
import jakarta.persistence.*;
import java.util.List;
@Entity class Book {
    @OneToMany List<Page> pages;
    @ManyToMany(fetch = FetchType.EAGER) List<Author> authors;
    List<Page> getPages() { return pages; }
    List<Author> getAuthors() { return authors; }
}
"#;
    let app = r#"package app;
import jakarta.persistence.EntityManager;
import java.util.List;
import model.Book;
class App {
    void report(EntityManager em) {
        var books = em.createQuery("select b from Book b", Book.class).getResultList();
        for (Book book : books) {
            print(book.getPages().size());
            print(book.getAuthors().size());
        }
        for (Book book : em.createQuery("select b from Book b join fetch b.pages", Book.class).getResultList()) {
            print(book.getPages().size());
        }
        for (Book book : em.createQuery("from Book", Book.class).getResultList()) {
            var unused = book.getPages();
        }
    }
}
"#;
    let found = scan(
        &[("model/Book.java", model), ("app/App.java", app)],
        &["app/App.java"],
    );
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].location.range.start_line, 9);
    assert_eq!(
        found[0].related_locations.as_ref().unwrap()[0].path,
        "model/Book.java"
    );
    assert_eq!(
        found[0].related_locations.as_ref().unwrap()[1]
            .range
            .start_line,
        7
    );
}

#[test]
fn testify_range_comparison_requires_unmodified_known_slice_and_import() {
    let good = r#"package test
import check "github.com/stretchr/testify/assert"
type Items []int
func test(t interface{}) {
    var values Items
    for i, item := range values {
        switch i { default: check.Equal(t, item, values[i]) }
    }
}
"#;
    let found = scan(&[("slice.go", good)], &["slice.go"]);
    assert_eq!(found.iter().filter(|item| item.source.rule_id.as_deref() == Some("go-testify-range-element-self-comparison")).count(), 1, "{found:#?}");
    for replacement in [
        "values[i] = 3; check.Equal(t, item, values[i])",
        "i++; check.Equal(t, item, values[i])",
        "item = 3; check.Equal(t, item, values[i])",
        "alias := values; alias[i] = 3; check.Equal(t, item, values[i])",
        "modify(&values[i]); check.Equal(t, item, values[i])",
        "item.Mutate(); check.Equal(t, item, values[i])",
        "values[i].Mutate(); check.Equal(t, item, values[i])",
        "check.Equal(t, item, expected[i])",
    ] {
        let bad = good.replace("check.Equal(t, item, values[i])", replacement);
        let found = scan(&[("slice.go", &bad)], &["slice.go"]);
        assert!(!found.iter().any(|item| item.source.rule_id.as_deref() == Some("go-testify-range-element-self-comparison")), "{replacement}: {found:#?}");
    }
    for bad in [good.replace("type Items []int", "type Items string"), good.replace("github.com/stretchr/testify/assert", "example.test/assert"), good.replace("var values Items", "var values Items; check := fake"), good.replace("var values Items", "var values Items; values = other"), good.replace("var values Items", "var values Items; type Items string")] {
        let found = scan(&[("slice.go", &bad)], &["slice.go"]);
        assert!(!found.iter().any(|item| item.source.rule_id.as_deref() == Some("go-testify-range-element-self-comparison")), "{found:#?}");
    }
}
