use autoreview_dataflow::lower;
use autoreview_langsupport::{language_for_path, Language};

use super::dsl::{run_call_order_rules, run_taint_rules};
use super::functions::{function_declarations, go_functions, java_functions, rust_functions};
use super::go_practices::{run_append_shared_backing_array, run_loopvar_checks};
use super::{
    call_order_rules, read_and_parse, run_npe_risk, run_typed_nil_interface_return, taint_rules,
    AgentFinding, Cfg, Node, ParseCache, Path, PersistentSummaryCache, ResolvedRulePack, Stmt,
    SummaryCaches, SummarySource,
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
        taint: taint_rules::load_taint_rules(packs),
        call_sequences: call_order_rules::load_call_order_rules(packs),
        go_pre_1_22: crate::analyzers::practices::go_module_targets_pre_1_22(root),
        caches: SummaryCaches {
            parse: &parse,
            persistent: &persistent,
            java_kotlin_packages: Default::default(),
        },
    };
    let mut findings: Vec<_> = paths
        .iter()
        .filter_map(|path| review.check_file(path))
        .flatten()
        .collect();
    findings.extend(super::keyed_imports::run(root, paths, &review.call_sequences, &review.caches));
    persistent.save();
    findings
}

struct DataflowReview<'a> {
    root: &'a Path,
    taint: Vec<taint_rules::TaintRuleDef>,
    call_sequences: Vec<call_order_rules::CallOrderRuleDef>,
    go_pre_1_22: bool,
    caches: SummaryCaches<'a>,
}

impl DataflowReview<'_> {
    fn check_file(&self, path: &str) -> Option<Vec<AgentFinding>> {
        let language = language_for_path(Path::new(path))?;
        let (extract, lower) = profile(language);
        let parsed = read_and_parse(self.root, path, language, self.caches.parse)?;
        let (content, tree) = parsed.as_ref();
        let lowered: Vec<_> = extract(tree)
            .into_iter()
            .map(|node| (node, lower(content.as_bytes(), node)))
            .collect();
        let file = SummarySource {
            root: self.root,
            path,
            content,
            caches: &self.caches,
        };
        let mut findings = language_checks(&file, language, &lowered);
        findings.extend(run_taint_rules(&file, language, &lowered, &self.taint));
        if language == Language::Go && self.go_pre_1_22 {
            findings.extend(run_loopvar_checks(path, &lowered));
        }
        findings.extend(run_call_order_rules(
            &file,
            language,
            &lowered,
            &self.call_sequences,
        ));
        Some(findings)
    }
}

fn language_checks(
    file: &SummarySource,
    language: Language,
    lowered: &[(Node<'_>, Cfg<Stmt>)],
) -> Vec<AgentFinding> {
    match language {
        Language::Go => {
            let mut findings = run_append_shared_backing_array(file.path, lowered);
            findings.extend(run_typed_nil_interface_return(file, lowered));
            findings
        }
        Language::Java => run_npe_risk(file, lowered, &[]),
        // Kotlin's stdlib supplies this nullable-receiver extension.
        Language::Kotlin => run_npe_risk(file, lowered, &["toString"]),
        _ => Vec::new(),
    }
}

type Extract = fn(&tree_sitter::Tree) -> Vec<Node<'_>>;
type Lower = fn(&[u8], Node<'_>) -> Cfg<Stmt>;

pub(super) fn profile(language: Language) -> (Extract, Lower) {
    match language {
        Language::Go => (go_functions, lower::go::lower_function),
        Language::Java => (java_functions, lower::java::lower_function),
        Language::Kotlin => (function_declarations, lower::kotlin::lower_function),
        Language::TypeScript | Language::Tsx => {
            (function_declarations, lower::javascript::lower_function)
        }
        Language::JavaScript => (function_declarations, lower::javascript::lower_function),
        Language::Rust => (rust_functions, lower::rust::lower_function),
    }
}
