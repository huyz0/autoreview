use autoreview_core::run_framework_checks;

fn count(path: &str, source: &str) -> usize {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join(path), source).unwrap();
    run_framework_checks(repo.path(), &[path.into()]).iter().filter(|finding|
        finding.source.rule_id.as_deref().is_some_and(|id| id.ends_with("webcrypto-digest-promise-as-array"))).count()
}

#[test]
fn digest_promise_used_as_typed_array_across_web_languages() {
    for path in ["test.js", "test.ts", "test.tsx"] {
        assert_eq!(count(path, "async function f(data) { const hash = crypto.subtle.digest('SHA-1', data); return new Uint8Array(hash); }"), 1, "{path}");
        assert_eq!(count(path, "async function f(data) { return new Uint8Array(crypto.subtle.digest('SHA-1', data)); }"), 1, "{path}");
        for safe in [
            "async function f(data) { const hash = await crypto.subtle.digest('SHA-1', data); return new Uint8Array(hash); }",
            "async function f(data) { const hash = crypto.subtle.digest('SHA-1', data); return await hash; }",
            "function f(crypto, data) { const hash = crypto.subtle.digest('SHA-1', data); return new Uint8Array(hash); }",
            "function f(Uint8Array, data) { const hash = crypto.subtle.digest('SHA-1', data); return new Uint8Array(hash); }",
            "function f(data) { const hash = crypto.subtle.digest('SHA-1', data); return Promise.all([hash]); }",
            "function f(data) { const hash = crypto.subtle.digest('SHA-1', data); observe(hash); return new Uint8Array(hash); }",
            "function f(data) { crypto.subtle.digest = custom; const hash = crypto.subtle.digest('SHA-1', data); return new Uint8Array(hash); }",
        ] { assert_eq!(count(path, safe), 0, "{path}: {safe}"); }
    }
}
