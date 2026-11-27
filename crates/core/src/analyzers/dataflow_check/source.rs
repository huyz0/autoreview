use super::{
    add_java_kotlin_summaries_from_file, go_imported_package_summaries, go_package_summaries,
    java_kotlin_imported_package_summaries, java_kotlin_package_summaries, Cfg, HashMap, Node,
    Path, Stmt, SummaryCaches,
};

pub(super) struct SummarySource<'a> {
    pub root: &'a Path,
    pub path: &'a str,
    pub content: &'a str,
    pub caches: &'a SummaryCaches<'a>,
}

impl SummarySource<'_> {
    pub(super) fn go_summaries(
        &self,
        filter: Option<fn(Node) -> bool>,
        compute: &dyn Fn(&Cfg<Stmt>) -> bool,
        rule_id: &str,
    ) -> HashMap<String, bool> {
        let mut summaries =
            go_package_summaries(self.root, self.path, filter, compute, rule_id, self.caches);
        summaries.extend(go_imported_package_summaries(
            self.root,
            self.path,
            filter,
            compute,
            rule_id,
            self.caches,
        ));
        summaries
    }

    pub(super) fn java_kotlin_summaries(
        &self,
        compute: &dyn Fn(&Cfg<Stmt>) -> bool,
        rule_id: &str,
    ) -> HashMap<String, bool> {
        let parsed = autoreview_langsupport::language_for_path(Path::new(self.path))
            .and_then(|language| self.caches.parse.get(&self.root.join(self.path), language));
        let package = parsed.as_ref().map(|parsed| super::jvm_index::package(parsed.1.root_node(), self.content));
        let mut summaries = match package {
            Some(super::jvm_index::Package::Named(package)) => java_kotlin_package_summaries(
                self.root,
                self.path,
                &package,
                compute,
                rule_id,
                self.caches,
            ),
            other => {
                if matches!(other, Some(super::jvm_index::Package::Unresolved)) {
                    self.caches.report_entry_package_issue(self.path, rule_id);
                }
                let mut own_only = HashMap::new();
                add_java_kotlin_summaries_from_file(
                    &self.root.join(self.path),
                    &mut own_only,
                    compute,
                    rule_id,
                    self.caches,
                );
                own_only
            }
        };
        summaries.extend(java_kotlin_imported_package_summaries(
            self.root,
            self.content,
            compute,
            rule_id,
            self.caches,
        ));
        summaries
    }
}
