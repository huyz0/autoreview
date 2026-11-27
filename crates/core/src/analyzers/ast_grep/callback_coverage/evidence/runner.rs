//! Explicitly configured experimental compiler probe. Its output remains diagnostic.
use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

const SNAPSHOT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/kotlin-callback-evidence/snapshot.py"));
const MAX_CONFIG: u64 = 256 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Config {
    python: PathBuf,
    kotlinc: PathBuf,
    plugin: PathBuf,
    jvm_target: String,
    sources: Vec<PathBuf>,
    jars: Vec<PathBuf>,
    #[serde(default)]
    runtime_jars: Vec<PathBuf>,
    #[serde(default)]
    source_roots: Vec<PathBuf>,
    #[serde(default)]
    required_classes: Vec<String>,
    #[serde(default)]
    trust_declared_build_inputs: bool,
}

pub(super) struct Output { pub(super) bytes: Vec<u8>, pub(super) complete_roots: bool }

pub(super) fn run(root: &Path, config: &Path) -> Result<Output, ()> {
    let config = read_config(config)?;
    run_config(root, config)
}

fn read_config(config: &Path) -> Result<Config, ()> {
    if !config.is_absolute() { return Err(()); }
    let mut input = Vec::new();
    std::fs::File::open(config).map_err(|_| ())?.take(MAX_CONFIG + 1).read_to_end(&mut input).map_err(|_| ())?;
    if input.len() as u64 > MAX_CONFIG { return Err(()); }
    serde_json::from_slice(&input).map_err(|_| ())
}

fn run_config(root: &Path, config: Config) -> Result<Output, ()> {
    let root = root.canonicalize().map_err(|_| ())?;
    let sources = sources(&root, &config.sources)?;
    if !config.source_roots.is_empty() { verify_source_roots(&root, &config.source_roots, &sources)?; }
    let jars = absolute_files(&config.jars, Some("jar"))?;
    let runtime = if config.runtime_jars.is_empty() { Vec::new() } else { absolute_files(&config.runtime_jars, Some("jar"))? };
    let python = absolute_file(&config.python, None)?;
    absolute_file(&config.kotlinc, None)?;
    let plugin = absolute_file(&config.plugin, Some("jar"))?;
    let temporary = tempfile::tempdir().map_err(|_| ())?;
    let script = temporary.path().join("snapshot.py");
    let output = temporary.path().join("evidence.json");
    std::fs::write(&script, SNAPSHOT).map_err(|_| ())?;
    let mut command = Command::new(python);
    command.arg(script).arg("--kotlinc").arg(&config.kotlinc).arg("--plugin").arg(&plugin)
        .arg("--jvm-target").arg(config.jvm_target).arg("--output").arg(&output);
    for source in &sources { command.arg("--source").arg(source); }
    for jar in &jars { command.arg("--jar").arg(jar); }
    for jar in &runtime { command.arg("--runtime-jar").arg(jar); }
    add_required_classes(&mut command, &config.required_classes)?;
    crate::analyzers::typescript_checker::run_bounded(&mut command, Vec::new(), Duration::from_secs(150)).ok_or(())?;
    if !config.source_roots.is_empty() { verify_source_roots(&root, &config.source_roots, &sources)?; }
    let mut bytes = Vec::new();
    std::fs::File::open(output).map_err(|_| ())?.take(super::MAX_REPORT + 1).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() as u64 > super::MAX_REPORT { return Err(()); }
    verify_manifest(&sources, &jars, &runtime, &plugin, &bytes)?;
    Ok(Output { bytes, complete_roots: !config.source_roots.is_empty() && !runtime.is_empty() && config.trust_declared_build_inputs })
}

fn add_required_classes(command: &mut Command, classes: &[String]) -> Result<(), ()> {
    if classes.len() > 128 || classes.iter().any(|name| name.len() > 1024) { return Err(()); }
    for class in classes { command.arg("--require-class").arg(class); }
    Ok(())
}

#[derive(Deserialize)]
struct Manifest { sources: Vec<super::Source>, dependencies: Vec<super::Source>, #[serde(default, rename = "runtimeDependencies")] runtime_dependencies: Vec<super::Source>, plugin: super::Source }

fn verify_manifest(sources: &[PathBuf], jars: &[PathBuf], runtime: &[PathBuf], plugin: &Path, report: &[u8]) -> Result<(), ()> {
    let manifest: Manifest = serde_json::from_slice(report).map_err(|_| ())?;
    verify_inputs(sources, manifest.sources, super::MAX_SOURCE, 16 * 1024 * 1024)?;
    verify_inputs(jars, manifest.dependencies, 64 * 1024 * 1024, 256 * 1024 * 1024)?;
    verify_inputs(runtime, manifest.runtime_dependencies, 64 * 1024 * 1024, 256 * 1024 * 1024)?;
    verify_inputs(&[plugin.to_path_buf()], vec![manifest.plugin], 64 * 1024 * 1024, 64 * 1024 * 1024)
}

fn verify_inputs(paths: &[PathBuf], records: Vec<super::Source>, limit: usize, total_limit: usize) -> Result<(), ()> {
    if records.len() != paths.len() { return Err(()); }
    let expected: HashSet<_> = paths.iter().cloned().collect();
    let mut seen = HashSet::new();
    let mut total = 0usize;
    for source in records {
        let path = source.path.canonicalize().map_err(|_| ())?;
        if !expected.contains(&path) || !seen.insert(path.clone()) { return Err(()); }
        let mut content = Vec::new();
        std::fs::File::open(path).map_err(|_| ())?.take((limit + 1) as u64).read_to_end(&mut content).map_err(|_| ())?;
        total = total.checked_add(content.len()).ok_or(())?;
        if content.len() > limit || total > total_limit || format!("{:x}", Sha256::digest(&content)) != source.sha256 { return Err(()); }
    }
    Ok(())
}

fn sources(root: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>, ()> {
    if paths.is_empty() || paths.len() > 512 { return Err(()); }
    let mut seen = HashSet::new();
    paths.iter().map(|path| {
        if !path.components().all(|part| matches!(part, Component::Normal(_))) { return Err(()); }
        let file = root.join(path).canonicalize().map_err(|_| ())?;
        if !file.starts_with(root) || !file.is_file() || file.extension().is_none_or(|ext| ext != "kt") || !seen.insert(file.clone()) { return Err(()); }
        Ok(file)
    }).collect()
}

fn absolute_files(paths: &[PathBuf], extension: Option<&str>) -> Result<Vec<PathBuf>, ()> {
    if paths.is_empty() || paths.len() > 512 { return Err(()); }
    let mut seen = HashSet::new();
    paths.iter().map(|path| {
        let file = absolute_file(path, extension)?;
        if !seen.insert(file.clone()) { return Err(()); }
        Ok(file)
    }).collect()
}

fn verify_source_roots(root: &Path, roots: &[PathBuf], sources: &[PathBuf]) -> Result<(), ()> {
    let mut pending = validate_roots(root, roots)?;
    let mut directories = HashSet::new();
    let mut found = HashSet::new();
    while let Some(directory) = pending.pop() {
        if !directories.insert(directory.clone()) || directories.len() > 4096 { return Err(()); }
        scan_directory(&directory, &mut pending, &mut found)?;
    }
    let expected: HashSet<_> = sources.iter().cloned().collect();
    if found != expected { return Err(()); }
    Ok(())
}

fn validate_roots(root: &Path, roots: &[PathBuf]) -> Result<Vec<PathBuf>, ()> {
    if roots.len() > 64 { return Err(()); }
    let mut pending = Vec::new();
    for path in roots {
        if !path.components().all(|part| matches!(part, Component::Normal(_))) { return Err(()); }
        let directory = root.join(path).canonicalize().map_err(|_| ())?;
        if !directory.starts_with(root) || !directory.is_dir() { return Err(()); }
        pending.push(directory);
    }
    Ok(pending)
}

fn scan_directory(directory: &Path, pending: &mut Vec<PathBuf>, found: &mut HashSet<PathBuf>) -> Result<(), ()> {
    for entry in std::fs::read_dir(directory).map_err(|_| ())? {
        let entry = entry.map_err(|_| ())?;
        let kind = entry.file_type().map_err(|_| ())?;
        if kind.is_symlink() { return Err(()); }
        if kind.is_dir() { pending.push(entry.path()); }
        else if kind.is_file() { collect_source(&entry.path(), found)?; }
        else { return Err(()); }
    }
    Ok(())
}

fn collect_source(path: &Path, found: &mut HashSet<PathBuf>) -> Result<(), ()> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("java" | "class" | "kts" | "scala" | "groovy") => Err(()),
        Some("kt") if !found.insert(path.canonicalize().map_err(|_| ())?) || found.len() > 512 => Err(()),
        _ => Ok(()),
    }
}

fn absolute_file(path: &Path, extension: Option<&str>) -> Result<PathBuf, ()> {
    if !path.is_absolute() { return Err(()); }
    let file = path.canonicalize().map_err(|_| ())?;
    if !file.is_file() || extension.is_some_and(|ext| file.extension().is_none_or(|actual| actual != ext)) { return Err(()); }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_paths_must_be_unique_and_inside_snapshot() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.kt"), "fun a() {}\n").unwrap();
        let canonical = root.path().canonicalize().unwrap();
        assert_eq!(sources(&canonical, &["a.kt".into()]).unwrap().len(), 1);
        assert!(sources(&canonical, &["../a.kt".into()]).is_err());
        assert!(sources(&canonical, &["a.kt".into(), "a.kt".into()]).is_err());
        assert!(sources(&canonical, &[canonical.join("a.kt")]).is_err());
    }

    #[test]
    fn source_roots_reject_omissions_and_unmodeled_languages() {
        let root = tempfile::tempdir().unwrap();
        let source_root = root.path().join("src/main/kotlin");
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::write(source_root.join("a.kt"), "fun a() {}\n").unwrap();
        let canonical = root.path().canonicalize().unwrap();
        let files = sources(&canonical, &["src/main/kotlin/a.kt".into()]).unwrap();
        let roots = vec!["src/main/kotlin".into()];
        assert!(verify_source_roots(&canonical, &roots, &files).is_ok());
        std::fs::write(source_root.join("b.kt"), "fun b() {}\n").unwrap();
        assert!(verify_source_roots(&canonical, &roots, &files).is_err());
        std::fs::remove_file(source_root.join("b.kt")).unwrap();
        std::fs::write(source_root.join("Unexpected.java"), "class Unexpected {}\n").unwrap();
        assert!(verify_source_roots(&canonical, &roots, &files).is_err());
    }

    #[test]
    fn manifest_rejects_changed_or_missing_sources() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a.kt");
        let source = b"fun a() {}\n";
        std::fs::write(&path, source).unwrap();
        let files = vec![path.canonicalize().unwrap()];
        let record = || super::super::Source { path: path.clone(), sha256: format!("{:x}", Sha256::digest(source)) };
        assert!(verify_inputs(&files, vec![record()], 1024, 1024).is_ok());
        assert!(verify_inputs(&[], vec![record()], 1024, 1024).is_err());
        std::fs::write(&path, "fun b() {}\n").unwrap();
        assert!(verify_inputs(&files, vec![record()], 1024, 1024).is_err());
    }

    #[test]
    fn configured_historical_probe_uses_snapshot_sources() {
        let (Ok(root), Ok(config)) = (std::env::var("AUTOREVIEW_TEST_CALLBACK_ROOT"), std::env::var("AUTOREVIEW_TEST_CALLBACK_CONFIG")) else { return };
        let root = Path::new(&root);
        let output = run(root, Path::new(&config)).unwrap();
        assert!(output.complete_roots);
        let value: serde_json::Value = serde_json::from_slice(&output.bytes).unwrap();
        let records = value["evidence"]["records"].as_array().unwrap();
        assert!(records.iter().any(|record| record["callee"] == "org.apache.sshd.server.SshServer.setPasswordAuthenticator"));
        assert_eq!(value["resolvedClasses"].as_array().unwrap().len(), 4);
        assert_eq!(value["runtimeResolvedClasses"].as_array().unwrap().len(), 4);
        assert!(super::super::parse(root, root, &output.bytes).is_ok());
        assert!(super::super::parse_with_trust(root, root, &output.bytes, true).unwrap().sshd_contract);
    }
}
