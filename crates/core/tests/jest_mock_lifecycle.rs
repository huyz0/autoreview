use autoreview_core::run_framework_checks;

#[test]
fn jest_spies_track_early_exit_finally_and_aliases() {
    let repo = tempfile::tempdir().unwrap();
    let path = "suite.test.ts";
    std::fs::write(repo.path().join(path), r#"
import { jest as mocks, test } from '@jest/globals';

test('early exit', () => {
    const spy = mocks.spyOn(subject, 'send');
    if (stop) return;
    spy.mockRestore();
});

test('restored by alias', () => {
    const spy = mocks.spyOn(subject, 'save');
    const owned = spy;
    owned.mockRestore();
});

test('finally', () => {
    const spy = mocks.spyOn(subject, 'load');
    try {
        if (stop) return;
        execute();
    } finally {
        spy.mockRestore();
    }
});
"#).unwrap();
    let found = run_framework_checks(repo.path(), &[path.into()]);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].source.rule_id.as_deref(), Some("jest-spy-not-restored"));
    assert_eq!(found[0].location.snippet, "mocks.spyOn(subject, 'send')");
}

#[test]
fn local_hooks_and_config_can_own_spy_restoration() {
    let repo = tempfile::tempdir().unwrap();
    let path = "suite.test.js";
    std::fs::write(repo.path().join(path), r#"
import { describe, jest, test, afterEach } from '@jest/globals';

describe('has hook', () => {
    afterEach(() => jest.restoreAllMocks());
    test('scoped', () => { const spy = jest.spyOn(subject, 'covered'); });
});
describe('no hook', () => {
    test('leaked', () => { const spy = jest.spyOn(subject, 'uncovered'); });
});
"#).unwrap();
    let changed = [path.into()];
    let found = run_framework_checks(repo.path(), &changed);
    assert_eq!(found.len(), 1);
    assert!(found[0].location.snippet.contains("'uncovered'"));
    std::fs::write(repo.path().join("package.json"), r#"{"jest": {"restoreMocks": true}}"#).unwrap();
    assert!(run_framework_checks(repo.path(), &changed).is_empty());
}

#[test]
fn scoped_spies_use_explicit_resource_management() {
    let repo = tempfile::tempdir().unwrap();
    let path = "scope.test.ts";
    std::fs::write(repo.path().join(path), r#"
import { jest, test } from '@jest/globals';

test('scoped', () => {
    using spy = jest.spyOn(subject, 'scoped');
    const leaked = jest.spyOn(subject, 'leaked');
});
"#).unwrap();
    let found = run_framework_checks(repo.path(), &[path.into()]);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].location.snippet.contains("'leaked'"));
}
