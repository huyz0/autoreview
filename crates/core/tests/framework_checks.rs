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
    assert_eq!(lines, [4, 5], "{found:#?}");
    let woven = format!(
        "import org.springframework.transaction.annotation.EnableTransactionManagement;\n{java}\n@EnableTransactionManagement(mode = AdviceMode.ASPECTJ) class Config {{}}"
    );
    assert!(scan(&[("Service.java", &woven)], &["Service.java"]).is_empty());
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
