//! Team event-log sync (`storage.sync.mode: git`): pushes/pulls this repo's
//! append-only event log (`<historyDir>/events/<date>-<host>.jsonl`)
//! through a dedicated orphan branch, so a team's rule/skill promotion
//! thresholds pool signal across contributors instead of each person
//! converging alone. Per the plan: because events are append-only and
//! keyed by host+day, sync is fetch-and-concatenate, never a real merge —
//! nobody edits another host's file, ever, so there's nothing to
//! conflict-resolve.
//!
//! Scope note, stated plainly: this brings synced event files onto disk
//! under this machine's `events/` directory, ready for a future
//! `history rebuild` to ingest into the SQLite index — that ingest path
//! doesn't exist yet (the index is populated directly from each run's own
//! `ReviewReport` today, a separate, pre-existing gap this task doesn't
//! attempt to close). Sync itself is real and independently useful:
//! teammates' event files land on disk and are inspectable/greppable even
//! before an ingest path exists to fold them into the index.

use std::path::{Path, PathBuf};
use std::process::Command;

use autoreview_schema::{StorageSyncConfig, SyncMode};

fn run_git(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        anyhow::bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Resolves `location` to an actual URL/path git can use as a remote: if
/// it's already a URL or filesystem path, it's passed through; if it looks
/// like a configured remote name in the *main* repo (e.g. `"origin"`), its
/// URL is looked up there — the sync repo is a separate local git working
/// copy, so it doesn't inherit the main repo's remotes automatically.
fn resolve_location(repo_root: &Path, location: &str) -> String {
    if location.contains('/') || location.contains(':') {
        return location.to_string();
    }
    run_git(repo_root, &["remote", "get-url", location]).unwrap_or_else(|_| location.to_string())
}

/// Ensures a local git working copy of the sync branch exists at
/// `sync_repo`, checked out to `branch` — creating an orphan branch if the
/// remote doesn't have one yet (first sync ever for this team).
fn ensure_sync_repo(repo_root: &Path, sync_repo: &Path, location: &str, branch: &str) -> anyhow::Result<()> {
    if !sync_repo.join(".git").exists() {
        std::fs::create_dir_all(sync_repo)?;
        run_git(sync_repo, &["init", "-q"])?;
        run_git(sync_repo, &["config", "user.email", "autoreview-sync@localhost"])?;
        run_git(sync_repo, &["config", "user.name", "autoreview-sync"])?;
        let url = resolve_location(repo_root, location);
        run_git(sync_repo, &["remote", "add", "origin", &url])?;
    }

    let fetched = run_git(sync_repo, &["fetch", "origin", branch]).is_ok();
    let current_branch = run_git(sync_repo, &["branch", "--show-current"]).unwrap_or_default();
    if current_branch == branch {
        if fetched {
            let _ = run_git(sync_repo, &["reset", "--hard", "FETCH_HEAD"]);
        }
        return Ok(());
    }

    if fetched && run_git(sync_repo, &["rev-parse", "--verify", "FETCH_HEAD"]).is_ok() {
        run_git(sync_repo, &["checkout", "-B", branch, "FETCH_HEAD"])?;
    } else {
        // No remote branch yet — this team's very first sync. `checkout
        // --orphan` fails if there are no commits at all yet in a brand
        // new repo, so make one empty commit as the orphan root first.
        run_git(sync_repo, &["checkout", "--orphan", branch])?;
        let _ = run_git(sync_repo, &["rm", "-rf", "--cached", "."]);
    }
    Ok(())
}

fn copy_event_files(from: &Path, to: &Path) -> anyhow::Result<usize> {
    if !from.exists() {
        return Ok(0);
    }
    std::fs::create_dir_all(to)?;
    let mut count = 0;
    for entry in std::fs::read_dir(from)?.filter_map(|e| e.ok()) {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("jsonl") {
            std::fs::copy(entry.path(), to.join(entry.file_name()))?;
            count += 1;
        }
    }
    Ok(count)
}

fn sync_repo_dir(history_dir: &Path) -> PathBuf {
    history_dir.join("sync-repo")
}

/// Pushes this machine's event log to the team's sync branch — best-effort,
/// per the plan ("push at the end of a run, never blocks the review on
/// network failure"): any error is swallowed, not propagated, so a diff run
/// on a flaky or offline network still succeeds. No-op when
/// `sync.mode != Git`.
pub fn sync_push(repo_root: &Path, history_dir: &Path, sync: &StorageSyncConfig) {
    if sync.mode != SyncMode::Git {
        return;
    }
    let _ = (|| -> anyhow::Result<()> {
        let location = sync.location.as_deref().unwrap_or("origin");
        let sync_repo = sync_repo_dir(history_dir);
        ensure_sync_repo(repo_root, &sync_repo, location, &sync.branch)?;
        copy_event_files(&history_dir.join("events"), &sync_repo.join("events"))?;
        run_git(&sync_repo, &["add", "-A"])?;
        // A no-op commit (nothing changed since last push) is expected and
        // fine to swallow — that's not a sync failure, just nothing new.
        let _ = run_git(&sync_repo, &["commit", "-q", "-m", "sync: update event logs"]);
        run_git(&sync_repo, &["push", "-q", "origin", &sync.branch])?;
        Ok(())
    })();
}

/// Pulls every host's event files reachable on the team's sync branch down
/// into this machine's local `events/` directory. Returns the count of
/// `.jsonl` files pulled — `0` (not an error) if `sync.mode != Git`, the
/// branch doesn't exist yet, or the remote is unreachable, so callers can
/// report "nothing synced" without treating it as a hard failure.
pub fn sync_pull(repo_root: &Path, history_dir: &Path, sync: &StorageSyncConfig) -> anyhow::Result<usize> {
    if sync.mode != SyncMode::Git {
        return Ok(0);
    }
    let location = sync.location.as_deref().unwrap_or("origin");
    let sync_repo = sync_repo_dir(history_dir);
    ensure_sync_repo(repo_root, &sync_repo, location, &sync.branch)?;
    copy_event_files(&sync_repo.join("events"), &history_dir.join("events"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_bare_remote(dir: &Path) {
        Command::new("git").args(["init", "--bare", "-q"]).current_dir(dir).status().unwrap();
    }

    fn write_event_file(events_dir: &Path, name: &str, content: &str) {
        std::fs::create_dir_all(events_dir).unwrap();
        std::fs::write(events_dir.join(name), content).unwrap();
    }

    fn sync_config(location: &str) -> StorageSyncConfig {
        StorageSyncConfig { mode: SyncMode::Git, location: Some(location.to_string()), branch: "autoreview-history".to_string() }
    }

    #[test]
    fn sync_push_is_a_no_op_when_mode_is_none() {
        let repo_root = tempfile::tempdir().unwrap();
        let history_dir = tempfile::tempdir().unwrap();
        let sync = StorageSyncConfig { mode: SyncMode::None, location: None, branch: "x".to_string() };
        sync_push(repo_root.path(), history_dir.path(), &sync);
        assert!(!sync_repo_dir(history_dir.path()).exists());
    }

    #[test]
    fn push_then_pull_from_a_different_machines_history_dir_round_trips_the_event_file() {
        let remote_dir = tempfile::tempdir().unwrap();
        init_bare_remote(remote_dir.path());
        let remote_url = remote_dir.path().to_string_lossy().to_string();

        // "Machine A" pushes its own event file.
        let repo_root_a = tempfile::tempdir().unwrap();
        let history_dir_a = tempfile::tempdir().unwrap();
        write_event_file(&history_dir_a.path().join("events"), "2026-07-14-hostA.jsonl", "{\"host\":\"hostA\"}\n");
        sync_push(repo_root_a.path(), history_dir_a.path(), &sync_config(&remote_url));

        // "Machine B" pulls and should see hostA's file even though it never wrote it.
        let repo_root_b = tempfile::tempdir().unwrap();
        let history_dir_b = tempfile::tempdir().unwrap();
        let pulled = sync_pull(repo_root_b.path(), history_dir_b.path(), &sync_config(&remote_url)).unwrap();
        assert_eq!(pulled, 1);
        assert!(history_dir_b.path().join("events").join("2026-07-14-hostA.jsonl").exists());
    }

    #[test]
    fn sync_pull_is_zero_when_mode_is_none() {
        let repo_root = tempfile::tempdir().unwrap();
        let history_dir = tempfile::tempdir().unwrap();
        let sync = StorageSyncConfig { mode: SyncMode::None, location: None, branch: "x".to_string() };
        assert_eq!(sync_pull(repo_root.path(), history_dir.path(), &sync).unwrap(), 0);
    }

    #[test]
    fn two_machines_pushing_different_hosts_both_land_on_a_third_machines_pull() {
        let remote_dir = tempfile::tempdir().unwrap();
        init_bare_remote(remote_dir.path());
        let remote_url = remote_dir.path().to_string_lossy().to_string();

        let repo_root_a = tempfile::tempdir().unwrap();
        let history_dir_a = tempfile::tempdir().unwrap();
        write_event_file(&history_dir_a.path().join("events"), "2026-07-14-hostA.jsonl", "{\"host\":\"hostA\"}\n");
        sync_push(repo_root_a.path(), history_dir_a.path(), &sync_config(&remote_url));

        let repo_root_b = tempfile::tempdir().unwrap();
        let history_dir_b = tempfile::tempdir().unwrap();
        write_event_file(&history_dir_b.path().join("events"), "2026-07-14-hostB.jsonl", "{\"host\":\"hostB\"}\n");
        // B must pull first (to not push a divergent history), then push its own file.
        sync_pull(repo_root_b.path(), history_dir_b.path(), &sync_config(&remote_url)).unwrap();
        sync_push(repo_root_b.path(), history_dir_b.path(), &sync_config(&remote_url));

        let repo_root_c = tempfile::tempdir().unwrap();
        let history_dir_c = tempfile::tempdir().unwrap();
        let pulled = sync_pull(repo_root_c.path(), history_dir_c.path(), &sync_config(&remote_url)).unwrap();
        assert_eq!(pulled, 2, "machine C should see both hostA's and hostB's event files");
    }
}
