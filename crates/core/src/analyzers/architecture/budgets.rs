use std::{collections::BTreeMap, path::Path};

use autoreview_archgraph::{
    build_go_import_graph, build_java_kotlin_import_graph, discover_go_module_path,
    go_imports_with_lines, go_package_for_file, java_kotlin_imports_with_lines,
    java_kotlin_package_for_file, ImportGraph,
};
use autoreview_schema::{AgentFinding, Severity};

use super::{architecture_finding, import_location, language_for_file, ImportLanguage};

pub(super) fn check(
    root: &Path,
    changed: &[String],
    budgets: &BTreeMap<String, usize>,
    go: &mut Option<(String, ImportGraph)>,
    jvm: &mut Option<ImportGraph>,
) -> Vec<AgentFinding> {
    if budgets.is_empty() {
        return Vec::new();
    }
    let module = discover_go_module_path(root);
    changed
        .iter()
        .filter_map(|path| {
            let language = language_for_file(path)?;
            let package = match language {
                ImportLanguage::Go => go_package_for_file(path, module.as_deref()?)?,
                ImportLanguage::JavaOrKotlin => java_kotlin_package_for_file(root, path)?,
                ImportLanguage::JavaScriptOrTypeScript => return None,
            };
            let &limit = budgets.get(&package)?;
            let graph = match language {
                ImportLanguage::Go => {
                    let module = module.as_deref()?;
                    &go.get_or_insert_with(|| (module.into(), build_go_import_graph(root, module)))
                        .1
                }
                ImportLanguage::JavaOrKotlin => {
                    jvm.get_or_insert_with(|| build_java_kotlin_import_graph(root))
                }
                ImportLanguage::JavaScriptOrTypeScript => return None,
            };
            if graph.fan_out(&package) <= limit {
                return None;
            }
            let content = std::fs::read_to_string(root.join(path)).ok()?;
            let imports = if language == ImportLanguage::Go {
                go_imports_with_lines(&content)
            } else {
                java_kotlin_imports_with_lines(&content)
            };
            finding(graph, &package, limit, path, &imports)
        })
        .collect()
}

fn finding(
    graph: &ImportGraph,
    package: &str,
    limit: usize,
    path: &str,
    imports: &[(String, u32)],
) -> Option<AgentFinding> {
    let edges = graph.edges.get(package)?;
    let mut evidence = imports
        .iter()
        .filter(|(import, _)| edges.values().any(|symbols| symbols.contains(import)));
    let (first_import, start_line) = evidence.next()?;
    let end_line = evidence.next_back().map_or(*start_line, |(_, line)| *line);
    let mut targets: Vec<_> = edges.keys().map(String::as_str).collect();
    targets.sort_unstable();
    let count = targets.len();
    let preview = targets.into_iter().take(5).collect::<Vec<_>>().join(", ");
    let remaining = if count > 5 {
        format!(" and {} more", count - 5)
    } else {
        String::new()
    };
    Some(architecture_finding(
        "internal-dependency-budget",
        Severity::Medium,
        "Package exceeds its internal dependency budget".into(),
        format!("`{package}` directly imports {count} internal packages. Its configured budget is {limit}. Dependencies include {preview}{remaining}."),
        import_location(path, *start_line..=end_line, first_import.clone()),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::{load_architecture_config, run_architecture_check};

    #[test]
    fn counts_distinct_internal_go_packages_and_anchors_changed_imports() {
        let root = tempfile::tempdir().unwrap();
        let fixtures = [
            ("go.mod", "module example.test/app\n"),
            ("handler/main.go", "package handler\nimport (\n\t\"example.test/app/a\"\n\t\"example.test/apple/external\"\n\t\"example.test/app/b\"\n)\n"),
            ("handler/other.go", "package handler\nimport \"example.test/app/a\"\n"),
            ("handler/empty.go", "package handler\n"),
            ("a/a.go", "package a\n"),
            ("b/b.go", "package b\n"),
            ("architecture.yaml", "architecture:\n  internalDependencyBudgets:\n    example.test/app/handler: 1\n"),
        ];
        for (path, contents) in fixtures {
            let full = root.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, contents).unwrap();
        }
        let config = load_architecture_config(&root.path().join("architecture.yaml"))
            .unwrap()
            .unwrap();
        let changed = ["handler/main.go".into(), "handler/empty.go".into()];
        let findings = run_architecture_check(root.path(), &changed, &config);
        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert_eq!(
            findings[0].source.rule_id.as_deref(),
            Some("internal-dependency-budget")
        );
        assert_eq!(findings[0].location.range.start_line, 3);
        assert_eq!(findings[0].location.range.end_line, Some(5));
        assert!(findings[0].message.contains("2 internal packages"));
    }

    #[test]
    fn honors_jvm_package_budgets_without_counting_multiple_types_in_one_package() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Handler.kt"),
            "package app.handler\nimport app.storage.Record\nimport app.storage.Store\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("Record.java"),
            "package app.storage;\nclass Record {}\nclass Store {}\n",
        )
        .unwrap();
        let changed = ["Handler.kt".to_owned()];
        let mut config = autoreview_schema::ArchitectureConfig::default();
        config
            .internal_dependency_budgets
            .insert("app.handler".into(), 1);
        assert!(run_architecture_check(root.path(), &changed, &config).is_empty());
        config
            .internal_dependency_budgets
            .insert("app.handler".into(), 0);
        let findings = run_architecture_check(root.path(), &changed, &config);
        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert!(findings[0].message.contains("app.storage"));
    }
}
