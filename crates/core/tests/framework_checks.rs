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
    let evidence: Vec<_> = found
        .iter()
        .map(|item| {
            (
                item.location.snippet.as_str(),
                item.related_locations.as_ref().unwrap()[0].path.as_str(),
            )
        })
        .collect();
    assert_eq!(
        evidence,
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
    let evidence: Vec<_> = found
        .iter()
        .map(|item| {
            (
                item.location.snippet.as_str(),
                item.related_locations.as_ref().unwrap()[0].path.as_str(),
            )
        })
        .collect();
    assert_eq!(
        evidence,
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
