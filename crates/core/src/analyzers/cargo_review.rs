//! Cargo metadata supplies package boundaries and actual build-script paths.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{ensure, Context};
use autoreview_langsupport::{parser_for, Language};
use autoreview_schema::{
    AgentFinding, ArchitectureConfig, FindingSource, FindingSourceKind, Location, LocationRange,
    Severity, Side,
};
use serde::Deserialize;
use toml_edit::{Document, Item};
use tree_sitter::Node;

use super::architecture::layer_for_path;
use super::rust_review::syntax::{children, descendants, is_comment, Source};

#[derive(Deserialize)]
pub struct CargoMetadata {
    pub workspace_members: HashSet<String>,
    pub packages: Vec<CargoPackage>,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
pub struct CargoPackage {
    pub id: String,
    pub name: String,
    pub manifest_path: PathBuf,
    dependencies: Vec<CargoDependency>,
    targets: Vec<CargoTarget>,
}

#[derive(Deserialize)]
struct CargoDependency {
    name: String,
    rename: Option<String>,
    kind: Option<String>,
    path: Option<PathBuf>,
    target: Option<String>,
    optional: bool,
}

#[derive(Deserialize)]
struct CargoTarget {
    kind: Vec<String>,
    src_path: PathBuf,
}

/// Reads workspace declarations without resolving external packages or running build scripts.
pub fn load_cargo_metadata(manifest: &Path) -> anyhow::Result<CargoMetadata> {
    // Cargo interprets a relative --manifest-path against the process's current directory.
    let manifest = manifest
        .canonicalize()
        .context("could not locate Cargo manifest")?;
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
            "--manifest-path",
        ])
        .arg(&manifest)
        .current_dir(
            manifest
                .parent()
                .context("Cargo manifest has no parent directory")?,
        )
        .output()
        .context("could not start cargo metadata")?;
    ensure!(
        output.status.success(),
        "cargo metadata failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    serde_json::from_slice(&output.stdout).context("could not read cargo metadata output")
}

#[derive(Default)]
pub struct CargoReview {
    pub findings: Vec<AgentFinding>,
    pub warnings: Vec<String>,
}

type ChangedPaths<'a> = BTreeMap<PathBuf, &'a str>;

pub fn run_cargo_review(
    repo_root: &Path,
    changed_files: &[String],
    architecture: Option<&ArchitectureConfig>,
) -> CargoReview {
    let mut review = CargoReview::default();
    let Ok(root) = repo_root.canonicalize() else {
        return review;
    };
    let changed: ChangedPaths<'_> = changed_files
        .iter()
        .filter(|path| {
            Path::new(path)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        })
        .filter_map(|path| {
            let requested = root.join(path);
            if !requested.symlink_metadata().ok()?.is_file() {
                return None;
            }
            in_repo_file(&root, &requested).map(|file| (file, path.as_str()))
        })
        .collect();
    if changed.is_empty() {
        return review;
    }
    let architecture =
        architecture.filter(|config| !config.layers.is_empty() && !config.rules.is_empty());
    let root_manifest = in_repo_file(&root, &root.join("Cargo.toml"));
    let nearest_manifests: BTreeSet<_> = changed
        .keys()
        .filter_map(|path| {
            path.parent()?
                .ancestors()
                .take_while(|directory| directory.starts_with(&root))
                .find_map(|directory| in_repo_file(&root, &directory.join("Cargo.toml")))
        })
        .collect();
    let mut seen_manifests = HashSet::new();
    let mut checked_scripts = HashSet::new();
    for manifest in root_manifest.into_iter().chain(nearest_manifests) {
        if !seen_manifests.insert(manifest.clone()) {
            continue;
        }
        let metadata = match load_cargo_metadata(&manifest) {
            Ok(metadata) => metadata,
            Err(error) => {
                if let Some(path) = relative_path(&root, &manifest) {
                    review.warnings.push(format!("{path}: {error:#}"));
                }
                continue;
            }
        };
        let packages: BTreeMap<_, _> = metadata
            .packages
            .iter()
            .filter(|package| metadata.workspace_members.contains(&package.id))
            .filter_map(|package| Some((in_repo_file(&root, &package.manifest_path)?, package)))
            .collect();
        seen_manifests.extend(packages.keys().cloned());
        review_build_scripts(
            &root,
            &changed,
            packages.values().copied(),
            &mut checked_scripts,
            &mut review,
        );
        if let Some(config) = architecture {
            review_layer_edges(&root, &changed, &metadata, &packages, config, &mut review);
        }
    }
    review
}

fn review_build_scripts<'a>(
    root: &Path,
    changed: &ChangedPaths<'_>,
    packages: impl Iterator<Item = &'a CargoPackage>,
    checked_scripts: &mut HashSet<PathBuf>,
    review: &mut CargoReview,
) {
    let targets = packages
        .flat_map(|package| &package.targets)
        .filter(|target| target.kind.iter().any(|kind| kind == "custom-build"));
    for target in targets {
        let Some(script) = in_repo_file(root, &target.src_path) else {
            continue;
        };
        let Some(path) = changed.get(&script) else {
            continue;
        };
        if !checked_scripts.insert(script.clone()) {
            continue;
        }
        match std::fs::read_to_string(script) {
            Ok(source) => review.findings.extend(build_script_findings(path, &source)),
            Err(error) => review
                .warnings
                .push(format!("{path}: could not read build script: {error}")),
        }
    }
}

fn in_repo_file(root: &Path, path: &Path) -> Option<PathBuf> {
    let path = path.canonicalize().ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    Some(path.strip_prefix(root).ok()?.to_str()?.replace('\\', "/"))
}

fn build_script_findings(path: &str, source: &str) -> Vec<AgentFinding> {
    let Some(tree) = parser_for(Language::Rust).and_then(|mut parser| parser.parse(source, None))
    else {
        return Vec::new();
    };
    let root = tree.root_node();
    if root.has_error() {
        return Vec::new();
    }
    let syntax = Source::new(root, source);
    descendants(root)
        .filter(|node| node.kind() == "macro_invocation")
        .filter_map(|node| {
            let variable = cargo_generated_trigger(literal_println(&syntax, node)?)?;
            Some(finding(
                "autoreview-cargo",
                "rust-cargo-generated-env-trigger",
                "correctness",
                Severity::Medium,
                format!("Rebuild trigger cannot track Cargo-generated {variable}"),
                format!(
                    "This directive watches the environment passed to Cargo. When Cargo supplies {variable} to the build script, changes to that generated value cannot trigger a rebuild through this directive."
                ),
                source_location(path, source, node.byte_range()),
                None,
            ))
        })
        .collect()
}

fn literal_println<'text>(source: &Source<'_, 'text>, node: Node<'_>) -> Option<&'text str> {
    if !source.standard_macro(node.child_by_field_name("macro")?, &["println"]) {
        return None;
    }
    let tokens = children(node).find(|child| child.kind() == "token_tree")?;
    let mut arguments = children(tokens).filter(|arg| !is_comment(*arg));
    let literal = arguments.next()?;
    if arguments.next().is_some() {
        return None;
    }
    let text = source.text(literal);
    match literal.kind() {
        "string_literal" => text.strip_prefix('"')?.strip_suffix('"'),
        "raw_string_literal" => {
            let (prefix, rest) = text.split_once('"')?;
            rest.strip_suffix(prefix.strip_prefix('r')?)?
                .strip_suffix('"')
        }
        _ => None,
    }
}

fn cargo_generated_trigger(text: &str) -> Option<&str> {
    let directive = text
        .strip_prefix("cargo::")
        .or_else(|| text.strip_prefix("cargo:"))?;
    let name = directive.strip_prefix("rerun-if-env-changed=")?;
    // Custom environment variables can share Cargo's prefixes. Without proving
    // a package's features or custom cfgs, only rely on documented names.
    let cargo_generated = matches!(
        name,
        "TARGET"
            | "HOST"
            | "OUT_DIR"
            | "OPT_LEVEL"
            | "DEBUG"
            | "PROFILE"
            | "NUM_JOBS"
            | "CARGO_MANIFEST_DIR"
            | "CARGO_MANIFEST_PATH"
            | "CARGO_CFG_FEATURE"
            | "CARGO_CFG_UNIX"
            | "CARGO_CFG_WINDOWS"
            | "CARGO_CFG_TARGET_FAMILY"
            | "CARGO_CFG_TARGET_OS"
            | "CARGO_CFG_TARGET_ARCH"
            | "CARGO_CFG_TARGET_VENDOR"
            | "CARGO_CFG_TARGET_ENV"
            | "CARGO_CFG_TARGET_ABI"
            | "CARGO_CFG_TARGET_POINTER_WIDTH"
            | "CARGO_CFG_TARGET_ENDIAN"
            | "CARGO_CFG_TARGET_FEATURE"
            | "CARGO_CFG_TARGET_HAS_ATOMIC"
            | "CARGO_PKG_VERSION"
            | "CARGO_PKG_VERSION_MAJOR"
            | "CARGO_PKG_VERSION_MINOR"
            | "CARGO_PKG_VERSION_PATCH"
            | "CARGO_PKG_VERSION_PRE"
            | "CARGO_PKG_AUTHORS"
            | "CARGO_PKG_NAME"
            | "CARGO_PKG_DESCRIPTION"
            | "CARGO_PKG_HOMEPAGE"
            | "CARGO_PKG_REPOSITORY"
            | "CARGO_PKG_LICENSE"
            | "CARGO_PKG_LICENSE_FILE"
            | "CARGO_PKG_RUST_VERSION"
            | "CARGO_PKG_README"
    );
    cargo_generated.then_some(name)
}

fn review_layer_edges(
    root: &Path,
    changed: &ChangedPaths<'_>,
    metadata: &CargoMetadata,
    packages: &BTreeMap<PathBuf, &CargoPackage>,
    config: &ArchitectureConfig,
    review: &mut CargoReview,
) {
    let workspace_manifest = in_repo_file(root, &metadata.workspace_root.join("Cargo.toml"));
    let workspace_document = workspace_manifest.as_ref().and_then(|path| {
        let changed_path = changed.get(path)?;
        Some((*changed_path, read_manifest(path, changed_path, review)?))
    });
    for (manifest, package) in packages {
        let changed_path = changed.get(manifest).copied();
        if changed_path.is_none() && workspace_document.is_none() {
            continue;
        }
        let Some(path) = relative_path(root, manifest) else {
            continue;
        };
        let Some(layer) = layer_for_path(&path, &config.layers) else {
            continue;
        };
        let forbidden: HashSet<_> = config
            .rules
            .iter()
            .filter(|rule| rule.forbid.from == layer)
            .flat_map(|rule| rule.forbid.to.iter().map(String::as_str))
            .collect();
        if forbidden.is_empty() {
            continue;
        }
        let Some(document) = read_manifest(manifest, &path, review) else {
            continue;
        };
        for dependency in package
            .dependencies
            .iter()
            .filter(|dependency| dependency.kind.is_none())
        {
            let Some(directory) = &dependency.path else {
                continue;
            };
            let Some(target) = in_repo_file(root, &directory.join("Cargo.toml")) else {
                continue;
            };
            let Some(target_package) = packages.get(&target) else {
                continue;
            };
            let Some(target_path) = relative_path(root, &target) else {
                continue;
            };
            let Some(target_layer) = layer_for_path(&target_path, &config.layers) else {
                continue;
            };
            if !forbidden.contains(target_layer) {
                continue;
            }
            let alias = dependency.rename.as_deref().unwrap_or(&dependency.name);
            let declaration = match dependency.target.as_deref() {
                Some(target) => target_dependency_entry(&document, target, alias),
                None => dependency_entry(document.as_item(), alias),
            };
            let Some((entry, range)) = declaration else {
                continue;
            };
            let inherited = entry.get("workspace").and_then(Item::as_bool) == Some(true);
            let workspace_location = workspace_document.as_ref().and_then(|(path, document)| {
                if !inherited {
                    return None;
                }
                let (_, range) = dependency_entry(document.get("workspace")?, alias)?;
                Some(source_location(path, document.raw(), range))
            });
            let member_location =
                source_location(changed_path.unwrap_or(&path), document.raw(), range);
            let (location, related) = match (changed_path, workspace_location) {
                (Some(_), related) => (member_location, related),
                (None, Some(location)) => (location, Some(member_location)),
                (None, None) => continue,
            };
            let optional = if dependency.optional { "optional " } else { "" };
            let target_condition = dependency
                .target
                .as_ref()
                .map_or_else(String::new, |target| format!(" for target {target}"));
            review.findings.push(finding(
                "autoreview-architecture",
                "rust-cargo-layer-violation",
                "architecture",
                Severity::High,
                format!("Cargo layer violation: {layer} -> {target_layer}"),
                format!(
                    "Cargo package {} declares the {optional}dependency {alias} on {}{target_condition}. The architecture policy forbids dependencies from {layer} to {target_layer}.",
                    package.name, target_package.name
                ),
                location,
                related,
            ));
        }
    }
}

fn read_manifest(path: &Path, label: &str, review: &mut CargoReview) -> Option<Document<String>> {
    let document = std::fs::read_to_string(path)
        .context("could not read Cargo manifest")
        .and_then(|source| Document::parse(source).context("could not parse Cargo manifest"));
    match document {
        Ok(document) => Some(document),
        Err(error) => {
            review.warnings.push(format!("{label}: {error:#}"));
            None
        }
    }
}

fn dependency_entry<'a>(section: &'a Item, alias: &str) -> Option<(&'a Item, Range<usize>)> {
    let table = section.get("dependencies")?.as_table_like()?;
    let entry = table.get(alias)?;
    let key_range = table.key(alias)?.span()?;
    let fields = entry
        .as_table_like()
        .into_iter()
        .flat_map(|table| table.iter().filter_map(|(_, field)| field.span()));
    let end = entry
        .span()
        .into_iter()
        .chain(fields)
        .fold(key_range.end, |end, range| end.max(range.end));
    Some((entry, key_range.start..end))
}

fn target_dependency_entry<'a>(
    document: &'a Document<String>,
    target: &str,
    alias: &str,
) -> Option<(&'a Item, Range<usize>)> {
    // Metadata normalizes cfg spacing. Match the declaration spelling while preserving
    // whitespace inside quoted values.
    document
        .get("target")?
        .as_table_like()?
        .iter()
        .filter(|(key, _)| target_tokens(key).eq(target_tokens(target)))
        .find_map(|(_, table)| dependency_entry(table, alias))
}

fn target_tokens(target: &str) -> impl Iterator<Item = char> + '_ {
    let mut quoted = false;
    let mut escaped = false;
    target.chars().filter(move |&character| {
        if escaped {
            escaped = false;
            return true;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            _ => {}
        }
        quoted || !character.is_ascii_whitespace()
    })
}

fn source_location(path: &str, source: &str, span: Range<usize>) -> Location {
    let line = |offset| {
        source.as_bytes()[..offset]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count() as u32
            + 1
    };
    Location {
        path: path.to_string(),
        range: LocationRange {
            start_line: line(span.start),
            end_line: Some(line(span.end.saturating_sub(1))),
            ..Default::default()
        },
        snippet: source[span].to_string(),
        side: Side::New,
    }
}

#[allow(clippy::too_many_arguments)]
fn finding(
    tool: &str,
    rule: &str,
    category: &str,
    severity: Severity,
    title: String,
    message: String,
    location: Location,
    related: Option<Location>,
) -> AgentFinding {
    AgentFinding {
        source: FindingSource {
            kind: FindingSourceKind::Analyzer,
            tool: tool.to_string(),
            rule_id: Some(rule.to_string()),
            aspect: None,
            backend: None,
        },
        category: category.to_string(),
        severity,
        confidence: 1.0,
        title,
        message,
        location,
        related_locations: related.map(|related| vec![related]),
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{ArchitectureForbidRule, ArchitectureLayer, ArchitectureRuleEntry};

    const BUILD_SCRIPT: &str = r##"fn main() {
    println!("cargo::rerun-if-env-changed=TARGET");
    std::println!(r#"cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH"#);
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo::rerun-if-env-changed=MY_TARGET");
    println!("cargo::rerun-if-env-changed=CARGO_PKG_BUILD_TAG");
    println!("cargo::rerun-if-env-changed=CARGO_CFG_CUSTOM_INPUT");
    println!("cargo::rerun-if-env-changed=CARGO_FEATURE_CUSTOM_INPUT");
    eprintln!("cargo::rerun-if-env-changed=OUT_DIR");
    println!("cargo::warning=rerun-if-env-changed=HOST");
    println!("cargo:rerun-if-env-changed={}", "DEBUG");
    log::println!("cargo::rerun-if-env-changed=PROFILE");
    let _ = "cargo::rerun-if-env-changed=HOST";
}
"##;

    fn write(root: &Path, path: &str, contents: &str) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, contents).unwrap();
    }

    fn package(root: &Path, directory: &str, manifest: &str) {
        write(root, &format!("{directory}/Cargo.toml"), manifest);
        write(root, &format!("{directory}/src/lib.rs"), "");
    }

    fn named_package(root: &Path, directory: &str, name: &str) {
        package(
            root,
            directory,
            &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        );
    }

    fn policy() -> ArchitectureConfig {
        ArchitectureConfig {
            internal_dependency_budgets: BTreeMap::new(),
            layers: vec![
                ArchitectureLayer {
                    name: "frontend".into(),
                    match_globs: vec!["apps/**".into()],
                },
                ArchitectureLayer {
                    name: "storage".into(),
                    match_globs: vec!["storage/**".into()],
                },
            ],
            rules: vec![ArchitectureRuleEntry {
                forbid: ArchitectureForbidRule {
                    from: "frontend".into(),
                    to: vec!["storage".into()],
                },
            }],
        }
    }

    #[test]
    fn matches_only_literal_directives_written_to_cargos_stdout() {
        let findings = build_script_findings("build.rs", BUILD_SCRIPT);
        assert_eq!(findings.len(), 2, "{findings:#?}");
        assert_eq!(findings[0].location.range.start_line, 2);
        assert_eq!(findings[1].location.range.start_line, 3);
        assert!(findings[1].title.ends_with("CARGO_CFG_TARGET_ARCH"));
    }

    #[test]
    fn distinguishes_standard_print_from_shadowed_macros() {
        let findings = build_script_findings(
            "build.rs",
            r#"use std::println as emit;
macro_rules! println { ($message:literal) => {}; }
fn main() {
    println!("cargo::rerun-if-env-changed=TARGET");
    emit!("cargo::rerun-if-env-changed=HOST");
}
"#,
        );
        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert!(findings[0].title.ends_with("HOST"));
    }

    #[test]
    fn recognizes_documented_package_and_cfg_names() {
        for name in [
            "CARGO_PKG_VERSION",
            "CARGO_PKG_README",
            "CARGO_CFG_TARGET_OS",
            "CARGO_CFG_TARGET_HAS_ATOMIC",
        ] {
            let directive = format!("cargo:rerun-if-env-changed={name}");
            assert_eq!(cargo_generated_trigger(&directive), Some(name));
        }
    }

    #[cfg(unix)]
    #[test]
    fn does_not_report_a_build_script_on_changed_symlink_lines() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        named_package(root, "actual", "actual");
        write(root, "actual/build.rs", BUILD_SCRIPT);
        std::os::unix::fs::symlink("actual/build.rs", root.join("alias.rs")).unwrap();

        let symlink_change = run_cargo_review(root, &["alias.rs".into()], None);
        assert!(symlink_change.findings.is_empty());
        assert!(symlink_change.warnings.is_empty());

        let source_change = run_cargo_review(root, &["actual/build.rs".into()], None);
        assert_eq!(source_change.findings.len(), 2);
        assert!(source_change.warnings.is_empty());
    }

    #[test]
    fn cargo_metadata_identifies_only_the_enabled_build_targets() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        write(
            root,
            "Cargo.toml",
            "[workspace]\nmembers = ['custom', 'default', 'off']\nresolver = '2'\n",
        );
        package(
            root,
            "custom",
            "[package]\nname = 'custom'\nversion = '0.1.0'\nbuild = 'tools/generate'\n",
        );
        named_package(root, "default", "default-build");
        package(
            root,
            "off",
            "[package]\nname = 'off'\nversion = '0.1.0'\nbuild = false\n",
        );
        for path in [
            "custom/tools/generate",
            "custom/build.rs",
            "default/build.rs",
            "off/build.rs",
            "orphan.rs",
        ] {
            write(root, path, BUILD_SCRIPT);
        }
        let changes = [
            "custom/tools/generate",
            "custom/build.rs",
            "default/build.rs",
            "off/build.rs",
            "orphan.rs",
            "../untracked.rs",
        ]
        .map(str::to_string);
        let review = run_cargo_review(root, &changes, None);
        assert!(review.warnings.is_empty(), "{:?}", review.warnings);
        assert_eq!(review.findings.len(), 4, "{:#?}", review.findings);
        let paths: HashSet<_> = review
            .findings
            .iter()
            .map(|finding| finding.location.path.as_str())
            .collect();
        assert_eq!(
            paths,
            HashSet::from(["custom/tools/generate", "default/build.rs"])
        );
        assert!(!root.join("Cargo.lock").exists());
    }

    #[test]
    fn reports_normal_workspace_edges_on_their_manifest_declarations() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        write(
            root,
            "Cargo.toml",
            r#"[workspace]
members = ["apps/ui", "storage/db", "storage/optional", "storage/jig"]
resolver = "2"

[workspace.dependencies]
persist = { package = "database-pkg", path = "storage/db" }
"#,
        );
        package(
            root,
            "apps/ui",
            r#"[package]
name = "ui"
version = "0.1.0"
edition = "2021"

[dependencies]
persist.workspace = true

[target.'cfg( windows )'.dependencies.condition]
package = "optional-path"
path = "../../storage/optional"
optional = true

[dev-dependencies]
jig = { path = "../../storage/jig" }

[build-dependencies]
jig = { path = "../../storage/jig" }
"#,
        );
        named_package(root, "storage/db", "database-pkg");
        named_package(root, "storage/optional", "optional-path");
        named_package(root, "storage/jig", "jig");
        let config = policy();
        let changes = ["apps/ui/Cargo.toml".to_string()];
        let review = run_cargo_review(root, &changes, Some(&config));
        assert!(review.warnings.is_empty(), "{:?}", review.warnings);
        assert_eq!(review.findings.len(), 2, "{:#?}", review.findings);
        let inherited = review
            .findings
            .iter()
            .find(|finding| finding.message.contains("database-pkg"))
            .unwrap();
        assert_eq!(inherited.location.path, "apps/ui/Cargo.toml");
        assert_eq!(inherited.location.range.start_line, 7);
        let conditional = review
            .findings
            .iter()
            .find(|finding| finding.message.contains("optional-path"))
            .unwrap();
        assert_eq!(conditional.location.range.start_line, 9);
        assert_eq!(conditional.location.range.end_line, Some(12));
        assert!(conditional
            .message
            .contains("optional dependency condition"));
        assert!(conditional.message.contains("cfg(windows)"));

        let only_workspace = run_cargo_review(root, &["Cargo.toml".to_string()], Some(&config));
        assert!(
            only_workspace.warnings.is_empty(),
            "{:?}",
            only_workspace.warnings
        );
        assert_eq!(
            only_workspace.findings.len(),
            1,
            "{:#?}",
            only_workspace.findings
        );
        let found = &only_workspace.findings[0];
        assert_eq!(found.location.path, "Cargo.toml");
        assert_eq!(found.location.range.start_line, 6);
        assert_eq!(
            found.related_locations.as_ref().unwrap()[0].path,
            "apps/ui/Cargo.toml"
        );

        let only_source =
            run_cargo_review(root, &["apps/ui/src/lib.rs".to_string()], Some(&config));
        assert!(only_source.findings.is_empty());
        let without_policy = run_cargo_review(root, &changes, None);
        assert!(without_policy.findings.is_empty());
    }

    #[test]
    fn reports_failed_metadata_and_keeps_other_workspaces_findings() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        write(root, "bad/Cargo.toml", "invalid TOML");
        write(root, "bad/build.rs", BUILD_SCRIPT);
        named_package(root, "good", "good");
        write(root, "good/build.rs", BUILD_SCRIPT);
        let changes = ["bad/build.rs", "good/build.rs"].map(str::to_string);
        let review = run_cargo_review(root, &changes, None);
        assert_eq!(review.warnings.len(), 1, "{:?}", review.warnings);
        assert!(review.warnings[0].starts_with("bad/Cargo.toml"));
        assert_eq!(review.findings.len(), 2);
        assert!(review
            .findings
            .iter()
            .all(|finding| finding.location.path == "good/build.rs"));
    }
}
