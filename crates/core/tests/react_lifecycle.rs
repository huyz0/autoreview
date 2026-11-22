use autoreview_core::run_framework_checks;

fn findings(source: &str) -> Vec<autoreview_schema::AgentFinding> {
    let repo = tempfile::tempdir().unwrap();
    let path = "View.tsx";
    std::fs::write(repo.path().join(path), source).unwrap();
    run_framework_checks(repo.path(), &[path.to_string()])
}

#[test]
fn interval_cleanup_tracks_aliases_and_all_cleanup_branches() {
    let findings = findings(r#"
import { useEffect as effect } from 'react';

effect(() => {
    const timer = setInterval(tick, 100);
    const alias = timer;
    return () => {
        if (enabled) clearInterval(alias);
        else clearInterval(timer);
    };
}, []);

effect(() => {
    const first = setInterval(tick, 100);
    const second = setInterval(tock, 200);
    return () => clearInterval(second);
}, []);

effect(() => {
    const timer = setInterval(tick, 100);
    return () => {
        if (stopEarly) return;
        clearInterval(timer);
    };
}, []);
"#);
    assert_eq!(findings.len(), 2);
    assert!(findings.iter().all(|finding| {
        finding.source.rule_id.as_deref() == Some("react-use-effect-interval-not-cleared")
            && finding.meta.as_ref().unwrap()["subcategory"][0] == "frontend.effect-cleanup"
            && finding.related_locations.as_ref().unwrap()[0].snippet.starts_with("setInterval")
    }));
}

#[test]
fn interval_cleanup_checks_setup_exits_and_declines_to_guess_about_helpers() {
    let findings = findings(r#"
import { useEffect } from 'react';

useEffect(() => {
    const id = setInterval(tick, 100);
    if (skip) return;
    return () => clearInterval(id);
}, []);

useEffect(() => {
    const id = setInterval(tick, 100);
    registerCleanup(id);
}, []);

useEffect(() => {
    const cleanup = () => clearInterval(id);
    const id = setInterval(tick, 100);
    return cleanup;
}, []);
"#);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].location.snippet, "return;");
}

#[test]
fn timer_checks_require_known_react_import_and_unshadowed_timer_functions() {
    for source in [r#"
import { useEffect } from 'other';
useEffect(() => { const id = setInterval(tick, 100); }, []);
"#, r#"
import { useEffect } from 'react';
function start(setInterval: () => string) {
    useEffect(() => { const id = setInterval(); }, []);
}
"#] {
        assert!(findings(source).is_empty());
    }
}
