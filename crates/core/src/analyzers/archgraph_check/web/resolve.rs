use std::path::{Component, Path, PathBuf};

use autoreview_langsupport::{language_for_path, Language};

pub(super) fn source_language(path: &Path) -> Option<Language> {
    let name = path.file_name()?.to_str()?;
    if [".d.ts", ".d.mts", ".d.cts"]
        .into_iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return None;
    }
    language_for_path(path).filter(|language| {
        matches!(
            language,
            Language::JavaScript | Language::TypeScript | Language::Tsx
        )
    })
}

pub(super) fn local_source(root: &Path, path: &Path) -> Option<String> {
    source_language(path)?;
    if !path.symlink_metadata().ok()?.is_file() {
        return None;
    }
    let resolved = path.canonicalize().ok()?;
    let relative = resolved.strip_prefix(root).ok()?;
    if relative.components().any(|part| {
        matches!(part, Component::Normal(name)
        if matches!(name.to_str(), Some("node_modules" | ".git" | "vendor")))
    }) {
        return None;
    }
    Some(relative.to_str()?.replace('\\', "/"))
}

pub(super) fn resolve(root: &Path, importer: &Path, specifier: &str) -> Option<String> {
    if !(specifier.starts_with("./") || specifier.starts_with("../"))
        || specifier.contains(['\\', '?', '#', '%', ':'])
    {
        return None;
    }
    let base = normalize(&importer.parent()?.join(specifier))?;
    let importer_ext = importer.extension()?.to_str()?;
    let absolute = root.join(&base);
    if base.extension().is_none() && absolute.is_file() {
        return None;
    }
    for candidate in candidates(
        &base,
        importer_ext,
        !absolute.join("package.json").is_file(),
    ) {
        let path = root.join(candidate);
        if path.is_file() {
            return local_source(root, &path);
        }
    }
    None
}

fn normalize(path: &Path) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => result.push(name),
            Component::CurDir => {}
            Component::ParentDir if result.pop() => {}
            _ => return None,
        }
    }
    Some(result)
}

fn candidates(base: &Path, importer: &str, allow_index: bool) -> Vec<PathBuf> {
    let typescript = matches!(importer, "ts" | "tsx" | "mts" | "cts");
    let extension = base.extension().and_then(|value| value.to_str());
    match (typescript, extension) {
        (true, Some("js")) => with_extensions(base, &["ts", "tsx", "js"]),
        (true, Some("jsx")) => with_extensions(base, &["tsx", "jsx"]),
        (true, Some("mjs")) => with_extensions(base, &["mts", "mjs"]),
        (true, Some("cjs")) => with_extensions(base, &["cts", "cjs"]),
        (_, None) if matches!(importer, "ts" | "tsx" | "cts" | "cjs" | "jsx") => {
            extensionless(base, importer, allow_index)
        }
        _ => vec![base.to_owned()],
    }
}

fn with_extensions(base: &Path, choices: &[&str]) -> Vec<PathBuf> {
    choices
        .iter()
        .map(|extension| base.with_extension(extension))
        .collect()
}

fn extensionless(base: &Path, importer: &str, allow_index: bool) -> Vec<PathBuf> {
    let choices: &[&str] = match importer {
        "cjs" => &["js", "json", "node"],
        "jsx" => &["js", "jsx", "json"],
        _ => &["ts", "tsx", "js", "jsx", "json"],
    };
    let mut candidates = with_extensions(base, choices);
    if allow_index {
        candidates.extend(with_extensions(&base.join("index"), choices));
    }
    candidates
}
