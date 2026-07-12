//! Shared fixture-repo builder for integration tests across the workspace.
//! Generates a throwaway 2-commit git repo on disk so tests exercise real
//! `git diff`/`git log` behavior rather than hand-built `DiffFacts`.

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct TestRepo {
    dir: tempfile::TempDir,
}

impl TestRepo {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git").args(args).current_dir(self.path()).output().expect("git must be on PATH to run these tests");
        assert!(output.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&output.stderr));
    }

    fn write_files(&self, files: &[(&str, &str)]) {
        for (name, contents) in files {
            let path: PathBuf = self.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
    }

    /// Commits `files` as a new commit on top of whatever's already there.
    pub fn commit(&self, files: &[(&str, &str)], message: &str) -> &Self {
        self.write_files(files);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self
    }
}

/// Builds a fresh git repo with one initial commit (`initial_files`) and
/// returns it positioned at `main` with that single commit — call
/// `.commit(...)` again to add the diff under test on top of it.
pub fn init_repo(initial_files: &[(&str, &str)]) -> TestRepo {
    let repo = TestRepo { dir: tempfile::tempdir().unwrap() };
    repo.git(&["init", "-q", "-b", "main"]);
    repo.git(&["config", "user.email", "test@example.com"]);
    repo.git(&["config", "user.name", "Test"]);
    repo.commit(initial_files, "initial");
    repo
}

/// Convenience for the common case: one base commit, one diff-under-test
/// commit on top, returning the repo ready for `autoreview diff --base
/// main~1 --head main`.
pub fn init_repo_with_diff(initial_files: &[(&str, &str)], diff_files: &[(&str, &str)], diff_message: &str) -> TestRepo {
    let repo = init_repo(initial_files);
    repo.commit(diff_files, diff_message);
    repo
}
