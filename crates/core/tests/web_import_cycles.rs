use autoreview_core::run_archgraph_check;

fn check(files: &[(&str, &str)], changed: &[&str]) -> Vec<autoreview_schema::AgentFinding> {
    let repo = tempfile::tempdir().unwrap();
    for (path, source) in files {
        let path = repo.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    let changed: Vec<_> = changed.iter().map(|path| (*path).to_string()).collect();
    run_archgraph_check(repo.path(), &changed)
}

#[test]
fn detects_return_paths_for_changed_edges_in_a_mixed_web_graph() {
    let found = check(
        &[
            (
                "src/view.tsx",
                "import { helper } from './helper.js';\nexport const view = <div>{helper}</div>;",
            ),
            (
                "src/helper.ts",
                "export { entry as helper } from './entry.mjs';",
            ),
            (
                "src/entry.mjs",
                "import './view.tsx';\nexport const entry = 1;",
            ),
            ("other/a.js", "import './b.js';"),
            ("other/b.js", "import './a.js';"),
        ],
        &["src/view.tsx", "src/helper.ts"],
    );
    assert_eq!(found.len(), 1, "{found:#?}");
    let finding = &found[0];
    assert_eq!(finding.source.rule_id.as_deref(), Some("web-import-cycle"));
    let related = finding.related_locations.as_ref().unwrap();
    assert_eq!(related.len(), 2);
    assert!(related.iter().any(|part| part.path == "src/entry.mjs"));
    assert!(related.iter().any(|part| part.path == "src/view.tsx"));
}

#[test]
fn ignores_type_only_dynamic_bare_and_nonlocal_edges() {
    let found = check(&[
        ("a.ts", "import type { B } from './b.js'; import { type C } from './c'; export { type D } from './d'; import('./e'); import 'f';"),
        ("b.ts", "import './a'; export type B = string;"),
        ("c.ts", "import './a'; export type C = string;"),
        ("d.ts", "import './a'; export type D = string;"),
        ("e.ts", "import './a';"),
        ("f.ts", "import './a';"),
        ("types.d.ts", "import './a';"),
        ("outside.ts", "import '../a';"),
    ], &["a.ts", "outside.ts"]);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn finds_static_side_effect_imports_and_type_mixed_barrels() {
    let found = check(
        &[
            ("main.ts", "import './cycle';"),
            (
                "cycle/index.ts",
                "export { type Item, functionValue } from '../impl.js';",
            ),
            (
                "impl.ts",
                "import './main'; export const functionValue = 0; export type Item = number;",
            ),
        ],
        &["main.ts"],
    );
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].location.range.start_line, 1);
    assert_eq!(
        found[0].related_locations.as_ref().unwrap()[0].path,
        "cycle/index.ts"
    );
}

#[test]
fn detects_eager_commonjs_cycles_and_skips_deferred_requires() {
    let found = check(
        &[
            ("loader.cjs", "const helper = require('./helper.cjs');"),
            ("helper.cjs", "require('./loader.cjs');"),
            (
                "lazy.cjs",
                "function work() { return require('./consumer.cjs'); }",
            ),
            ("consumer.cjs", "require('./lazy.cjs');"),
        ],
        &["loader.cjs", "lazy.cjs"],
    );
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].location.path, "loader.cjs");
}
