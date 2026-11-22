use autoreview_core::run_framework_checks;
use autoreview_test_support::init_repo;

#[test]
fn shutdown_hooks_join_the_same_executor_they_shut_down() {
    let path = "Worker.java";
    let source = r#"
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;

class Worker {
    final ExecutorService executor;
    final ExecutorService other;

    Worker(ExecutorService executor, ExecutorService other) {
        this.executor = executor;
        this.other = other;
    }

    void registersMethodReference() {
        Runtime.getRuntime().addShutdownHook(new Thread(executor::shutdown));
    }
    void registersExpressionLambda() {
        java.lang.Runtime.getRuntime().addShutdownHook(new java.lang.Thread(() -> this.executor.shutdown()));
    }
    void waitsForAnotherPool() {
        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            executor.shutdown();
            other.close();
        }));
    }
    void registersLocalPool() {
        var local = Executors.newSingleThreadExecutor();
        Runtime.getRuntime().addShutdownHook(new Thread(local::shutdown));
    }
    void waitsForOwnPool() {
        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            executor.shutdown();
            try {
                if (!this.executor.awaitTermination(10, TimeUnit.SECONDS)) executor.shutdownNow();
            } catch (InterruptedException interrupted) {
                executor.shutdownNow();
                Thread.currentThread().interrupt();
            }
        }));
    }
    void waitsByClosing() {
        Runtime.getRuntime().addShutdownHook(new Thread(executor::close));
    }
    void deliberatelyInterrupts() {
        Runtime.getRuntime().addShutdownHook(new Thread(() -> executor.shutdownNow()));
    }
    void ordinaryMethod() {
        executor.shutdown();
    }
    void unrelatedType(CustomExecutor executor) {
        Runtime.getRuntime().addShutdownHook(new Thread(executor::shutdown));
    }
}

"#;
    let repo = init_repo(&[(path, source)]);
    let findings = run_framework_checks(repo.path(), &[path.into()]);
    let shutdowns: Vec<_> = findings
        .iter()
        .filter(|finding| {
            finding.source.rule_id.as_deref() == Some("java-executor-shutdown-hook-without-await")
        })
        .collect();
    assert_eq!(shutdowns.len(), 4, "{findings:#?}");
    for (finding, expected) in shutdowns.iter().zip([
        "executor::shutdown",
        "this.executor.shutdown()",
        "executor.shutdown()",
        "local::shutdown",
    ]) {
        assert_eq!(finding.location.snippet, expected);
        let hook = &finding.related_locations.as_ref().unwrap()[0];
        assert!(hook.snippet.contains("addShutdownHook"));
    }
}

#[test]
fn shutdown_hook_wait_must_be_on_every_normal_exit_path() {
    let path = "Conditional.java";
    let source = r#"
import java.util.concurrent.ExecutorService;
class Conditional {
    void registers(ExecutorService worker, boolean wait) {
        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            worker.shutdown();
            if (wait) worker.close();
        }));
    }
    void completesFirst(ExecutorService worker) {
        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            worker.close();
            worker.shutdown();
        }));
    }
    void forceStop(ExecutorService worker) {
        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            worker.shutdown();
            worker.shutdownNow();
        }));
    }
}
"#;
    let repo = init_repo(&[(path, source)]);
    let findings = run_framework_checks(repo.path(), &[path.into()]);
    let finding = findings
        .iter()
        .find(|finding| {
            finding.source.rule_id.as_deref() == Some("java-executor-shutdown-hook-without-await")
        })
        .unwrap();
    assert_eq!(finding.location.range.start_line, 6);
    assert_eq!(findings.len(), 1, "{findings:#?}");
}
