use std::path::Path;

use autoreview_schema::AgentFinding;
use globset::{Glob, GlobSet, GlobSetBuilder};
use tree_sitter::Node;

pub(in crate::analyzers::framework_checks) struct Scope<'a, T> {
    paths: GlobSet,
    pub config: &'a T,
}

impl<'a, T> Scope<'a, T> {
    pub fn new(paths: &[String], config: &'a T) -> anyhow::Result<Self> {
        let mut globs = GlobSetBuilder::new();
        for path in paths {
            globs.add(Glob::new(path)?);
        }
        Ok(Self {
            paths: globs.build()?,
            config,
        })
    }

    pub fn matches(&self, path: &str) -> bool {
        self.paths.is_match(path)
    }
}

pub(in crate::analyzers::framework_checks) fn run<'a, T: 'a>(
    root: &Path,
    changed: &[String],
    policies: impl Iterator<Item = anyhow::Result<Scope<'a, T>>>,
    inspect: impl Fn(&str, &str, Node<'_>, &[Scope<'_, T>]) -> Vec<AgentFinding>,
) -> anyhow::Result<Vec<AgentFinding>> {
    let policies = policies.collect::<anyhow::Result<Vec<_>>>()?;
    let paths: Vec<_> = changed
        .iter()
        .filter(|path| policies.iter().any(|policy| policy.matches(path)))
        .cloned()
        .collect();
    Ok(super::run(root, &paths, |path, source, file| {
        inspect(path, source, file, &policies)
    }))
}
