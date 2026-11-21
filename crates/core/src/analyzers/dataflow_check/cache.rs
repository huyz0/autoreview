use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use tree_sitter::Tree;

/// A read+parse cache shared across one `run_dataflow_check` invocation —
/// Rule engine roadmap item 1 (see `RULE_ENGINE_RESEARCH.md`): every
/// interprocedural rule's same-package/cross-package resolution
/// (`package_summaries`, `imported_package_summaries`,
/// `npe_package_summaries`, `npe_imported_package_summaries`) previously
/// independently re-read and re-parsed the same sibling/imported files
/// from scratch, once per changed file that needed them — if N changed
/// files in one diff import the same package, that package's files got
/// parsed N times. This cache makes each file's read+parse pay its cost
/// at most once per `run_dataflow_check` call, regardless of how many
/// changed files or rules end up needing it. `Rc` rather than a plain
/// owned return because `Tree`'s clone is cheap (an internal refcount
/// bump, backed by tree-sitter's own C library) but re-parsing is not —
/// sharing one parse via `Rc` avoids paying for a second clone of the
/// tree on every cache hit.
pub(super) type ParsedFile = Rc<(String, Tree)>;

pub(super) struct ParseCache {
    entries: RefCell<HashMap<PathBuf, Option<ParsedFile>>>,
}

impl ParseCache {
    pub(super) fn new() -> Self {
        ParseCache {
            entries: RefCell::new(HashMap::new()),
        }
    }

    /// Returns the cached (content, tree) for `path`, parsing and caching
    /// it on first request. `None` on a read or parse failure — cached as
    /// `None` too, so a missing/unparseable file isn't retried on every
    /// subsequent lookup within this run.
    pub(super) fn get(
        &self,
        path: &Path,
        language: autoreview_langsupport::Language,
    ) -> Option<ParsedFile> {
        if let Some(cached) = self.entries.borrow().get(path) {
            return cached.clone();
        }
        let parsed = (|| {
            let content = std::fs::read_to_string(path).ok()?;
            let mut parser = autoreview_langsupport::parser_for(language)?;
            let tree = parser.parse(&content, None)?;
            Some(Rc::new((content, tree)))
        })();
        self.entries
            .borrow_mut()
            .insert(path.to_path_buf(), parsed.clone());
        parsed
    }
}

/// Rule engine roadmap item 5 (persistent cross-run summary cache, see
/// `RULE_ENGINE_RESEARCH.md`): `ParseCache` above (item 1) only dedupes
/// work *within* one `run_dataflow_check` call — a `diff --watch` session
/// or repeated CLI invocations against the same working tree still
/// rescan every sibling/imported file's summaries from scratch on every
/// call, even when that file hasn't changed since the last run. This
/// persists `(rule_id, file_path) -> (content_hash, per-function summary
/// map)` to one JSON file per repo under
/// `~/.cache/autoreview/dataflow-summaries/<fingerprint>.json`.
///
/// Content-hash keyed rather than mtime-keyed so it's correctness-neutral
/// by construction: a hit only ever returns a summary this exact
/// byte-for-byte file content has already produced, so a stale entry is
/// definitionally impossible — an edited file simply gets a different
/// hash and misses, recomputing exactly as if there were no cache at
/// all. `rule_id` is part of the key because two rules scanning the same
/// file want different summaries from it (e.g. "does this function ever
/// return a nil pointer" vs. "does this function ever return tainted
/// data") — folding them into one map keyed by function name alone would
/// let one rule's cached answer silently leak into another's lookup.
///
/// Best-effort throughout: a missing, corrupt, or unwritable cache file
/// degrades silently to an empty cache (everything recomputed, and
/// persisted normally on success) rather than a hard error — losing this
/// cache should never be able to break a scan, only slow it down.
pub(super) struct PersistentSummaryCache {
    cache_path: PathBuf,
    source_root: PathBuf,
    data: RefCell<PersistentSummaryCacheData>,
    dirty: RefCell<bool>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct PersistentSummaryCacheData {
    #[serde(default)]
    entries: HashMap<String, HashMap<String, CachedFileSummaries>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CachedFileSummaries {
    content_hash: String,
    summaries: HashMap<String, bool>,
}

impl PersistentSummaryCache {
    pub(super) fn load(repo_root: &Path, source_root: &Path) -> Self {
        let cache_path = summary_cache_path(repo_root);
        let data = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        PersistentSummaryCache {
            cache_path,
            source_root: source_root.to_path_buf(),
            data: RefCell::new(data),
            dirty: RefCell::new(false),
        }
    }

    pub(super) fn get(
        &self,
        rule_id: &str,
        path: &Path,
        content_hash: &str,
    ) -> Option<HashMap<String, bool>> {
        let data = self.data.borrow();
        let relative = path.strip_prefix(&self.source_root).ok()?;
        let entry = data
            .entries
            .get(rule_id)?
            .get(relative.to_string_lossy().as_ref())?;
        (entry.content_hash == content_hash).then(|| entry.summaries.clone())
    }

    pub(super) fn put(
        &self,
        rule_id: &str,
        path: &Path,
        content_hash: String,
        summaries: HashMap<String, bool>,
    ) {
        let Ok(relative) = path.strip_prefix(&self.source_root) else {
            return;
        };
        self.data
            .borrow_mut()
            .entries
            .entry(rule_id.to_string())
            .or_default()
            .insert(
                relative.to_string_lossy().into_owned(),
                CachedFileSummaries {
                    content_hash,
                    summaries,
                },
            );
        *self.dirty.borrow_mut() = true;
    }

    /// Writes the cache back to disk, but only if `put` actually added
    /// something new this run — an unchanged cache has nothing worth the
    /// write, and skipping it also means a read-only or missing
    /// `~/.cache` never turns into spurious write attempts on every scan.
    pub(super) fn save(&self) {
        if !*self.dirty.borrow() {
            return;
        }
        let Some(parent) = self.cache_path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(mut file) = tempfile::NamedTempFile::new_in(parent) else {
            return;
        };
        if serde_json::to_writer(&mut file, &*self.data.borrow()).is_ok() {
            let _ = file.persist(&self.cache_path);
        }
    }
}

/// One cache file per repo, named by a short hash of the repo's
/// canonicalized path — avoids collisions between unrelated repos
/// sharing the same cache directory without needing a full path (which
/// can contain characters unsafe for a filename) as the name itself.
pub(super) fn summary_cache_path(repo_root: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let canonical = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    let fingerprint: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("autoreview")
        .join("dataflow-summaries")
        .join(format!("{fingerprint}-v2.json"))
}

pub(super) fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The in-run and cross-run caches always travel together through every
/// summary-computation call below — bundled into one struct rather than
/// two separate parameters purely to stay under clippy's argument-count
/// lint on the functions that also take a rule id, package filter, and
/// summary closure.
pub(super) struct SummaryCaches<'a> {
    pub(super) parse: &'a ParseCache,
    pub(super) persistent: &'a PersistentSummaryCache,
    pub(super) java_kotlin_packages: OnceCell<HashMap<String, Vec<PathBuf>>>,
}
