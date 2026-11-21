use autoreview_langsupport::{language_for_path, Language};

use super::dsl::{run_call_order_rules, run_loaded_taint_rules};
use super::{
    call_order_rules, lower_all_functions, lower_all_java_functions,
    lower_all_javascript_functions, lower_all_kotlin_functions, read_and_parse,
    run_append_shared_backing_array, run_loopvar_checks, run_npe_risk,
    run_typed_nil_interface_return, AgentFinding, Cfg, Node, ParseCache, Path,
    PersistentSummaryCache, ResolvedRulePack, Stmt, SummaryCaches,
};

pub(super) fn run(
    repo: &Path,
    root: &Path,
    paths: &[String],
    packs: &[ResolvedRulePack],
) -> Vec<AgentFinding> {
    let parse = ParseCache::new();
    let persistent = PersistentSummaryCache::load(repo, root);
    let review = DataflowReview {
        root,
        packs,
        call_sequences: call_order_rules::load_call_order_rules(packs),
        go_pre_1_22: crate::analyzers::practices::go_module_targets_pre_1_22(root),
        caches: SummaryCaches {
            parse: &parse,
            persistent: &persistent,
            java_kotlin_packages: Default::default(),
        },
    };
    let findings = paths
        .iter()
        .filter_map(|path| review.check_file(path))
        .flatten()
        .collect();
    persistent.save();
    findings
}

struct DataflowReview<'a> {
    root: &'a Path,
    packs: &'a [ResolvedRulePack],
    call_sequences: Vec<call_order_rules::CallOrderRuleDef>,
    go_pre_1_22: bool,
    caches: SummaryCaches<'a>,
}

impl DataflowReview<'_> {
    fn check_file(&self, path: &str) -> Option<Vec<AgentFinding>> {
        let language = language_for_path(Path::new(path))?;
        let (declared_language, lower) = profile(language)?;
        let parsed = read_and_parse(self.root, path, language, self.caches.parse)?;
        let (content, tree) = parsed.as_ref();
        let lowered = lower(content.as_bytes(), tree);
        let mut findings = self.language_checks(path, content, language, &lowered);
        findings.extend(run_loaded_taint_rules(
            self.root,
            path,
            content,
            declared_language,
            &lowered,
            self.packs,
            &self.caches,
        ));
        if language == Language::Go && self.go_pre_1_22 {
            findings.extend(run_loopvar_checks(path, &lowered));
        }
        findings.extend(run_call_order_rules(
            path,
            declared_language,
            &lowered,
            &self.call_sequences,
        ));
        Some(findings)
    }

    fn language_checks(
        &self,
        path: &str,
        content: &str,
        language: Language,
        lowered: &[(Node<'_>, Cfg<Stmt>)],
    ) -> Vec<AgentFinding> {
        match language {
            Language::Go => {
                let mut findings = run_append_shared_backing_array(path, lowered);
                findings.extend(run_typed_nil_interface_return(
                    self.root,
                    path,
                    content.as_bytes(),
                    lowered,
                    &self.caches,
                ));
                findings
            }
            Language::Java => run_npe_risk(self.root, path, content, lowered, &[], &self.caches),
            // Kotlin's stdlib supplies this nullable-receiver extension.
            Language::Kotlin => run_npe_risk(
                self.root,
                path,
                content,
                lowered,
                &["toString"],
                &self.caches,
            ),
            _ => Vec::new(),
        }
    }
}

type Lower = for<'tree> fn(&[u8], &'tree tree_sitter::Tree) -> Vec<(Node<'tree>, Cfg<Stmt>)>;

fn profile(language: Language) -> Option<(&'static str, Lower)> {
    Some(match language {
        Language::Go => ("Go", lower_all_functions),
        Language::Java => ("Java", lower_all_java_functions),
        Language::Kotlin => ("Kotlin", lower_all_kotlin_functions),
        Language::TypeScript | Language::Tsx => ("TypeScript", lower_all_javascript_functions),
        Language::JavaScript => ("JavaScript", lower_all_javascript_functions),
        Language::Rust => return None,
    })
}
