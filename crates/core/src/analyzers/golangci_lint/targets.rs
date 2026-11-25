use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

pub(super) struct Group {
    pub root: PathBuf,
    pub prefix: PathBuf,
    pub files: Vec<String>,
    pub targets: Vec<String>,
}

/// Group files by their nearest Go module or workspace. Files outside a Go
/// project retain the existing named-file mode, grouped by directory.
pub(super) fn groups(repo: &Path, changed: &[String]) -> Vec<Group> {
    let mut grouped = BTreeMap::<(Option<PathBuf>, PathBuf), Vec<String>>::new();
    for file in changed {
        let relative = Path::new(file);
        if !file.ends_with(".go") || !relative.components().all(|part| matches!(part, Component::Normal(_))) || !repo.join(relative).is_file() { continue; }
        let parent = relative.parent().unwrap_or(Path::new(""));
        let context = parent.ancestors().find(|dir| repo.join(dir).join("go.mod").is_file() || repo.join(dir).join("go.work").is_file()).map(Path::to_path_buf);
        let directory = if context.is_some() { PathBuf::new() } else { parent.to_path_buf() };
        grouped.entry((context, directory)).or_default().push(file.clone());
    }
    grouped.into_iter().map(|((context, _), files)| make_group(repo, context, files)).collect()
}

fn make_group(repo: &Path, context: Option<PathBuf>, files: Vec<String>) -> Group {
    let Some(prefix) = context else { return Group { root: repo.to_path_buf(), prefix: PathBuf::new(), targets: files.clone(), files }; };
    let files: Vec<_> = files.into_iter().filter_map(|file| Path::new(&file).strip_prefix(&prefix).ok().map(normalize)).collect();
    let packages: BTreeSet<_> = files.iter().map(|file| {
        file.rsplit_once('/').map_or_else(|| ".".to_owned(), |(directory, _)| format!("./{directory}"))
    }).collect();
    Group { root: repo.join(&prefix), prefix, files, targets: packages.into_iter().collect() }
}

pub(super) fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/")
}
