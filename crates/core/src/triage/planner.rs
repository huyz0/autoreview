use globset::Glob;

use autoreview_schema::{
    AutoreviewConfig, PlanBudgets, ReviewPlan, SkillManifest, SpecialistPlanEntry, Tier, TriageSignalScore,
};

use super::signals::DiffFacts;

fn lines_changed(facts: &DiffFacts) -> f64 {
    facts.files.iter().map(|f| (f.additions + f.deletions) as f64).sum()
}

fn clamp(value: f64, cap: Option<f64>) -> f64 {
    match cap {
        Some(c) => value.min(c),
        None => value,
    }
}

/// Heuristic-only triage (Stage 2): config-weighted signals sum to a score,
/// the score picks a tier. No LLM classifier here yet (M2, ambiguity-band only).
/// `analyzer_finding_count` is the count of Stage-1 deterministic findings —
/// Stage 1 runs before Stage 2 specifically so this signal is available.
pub fn score_diff_facts(facts: &DiffFacts, config: &AutoreviewConfig, analyzer_finding_count: usize) -> (f64, Vec<TriageSignalScore>) {
    let w = &config.triage.signals;
    let mut signals = Vec::new();

    let mut push = |signal: &str, points: f64, detail: Option<String>| {
        if points > 0.0 {
            signals.push(TriageSignalScore { signal: signal.to_string(), points, detail });
        }
    };

    if let Some(weight) = w.get("linesChanged") {
        if let Some(per_line) = weight.per_line {
            push("linesChanged", clamp(lines_changed(facts) * per_line, weight.cap), None);
        }
    }
    if let Some(weight) = w.get("filesChanged") {
        if let Some(per_file) = weight.per_file {
            push("filesChanged", clamp(facts.files.len() as f64 * per_file, weight.cap), None);
        }
    }
    if facts.sensitive_path_hit {
        if let Some(weight) = w.get("sensitivePathHit") {
            if let Some(points) = weight.points {
                push("sensitivePathHit", points, Some(facts.sensitive_paths.join(", ")));
            }
        }
    }
    if facts.dependency_change {
        if let Some(weight) = w.get("dependencyChange") {
            if let Some(points) = weight.points {
                push("dependencyChange", points, None);
            }
        }
    }
    if facts.ci_or_infra_change {
        if let Some(weight) = w.get("ciOrInfraChange") {
            if let Some(points) = weight.points {
                push("ciOrInfraChange", points, None);
            }
        }
    }
    if let Some(weight) = w.get("complexityDelta") {
        if let Some(per_branch) = weight.per_branch {
            push("complexityDelta", clamp(facts.added_branch_keywords as f64 * per_branch, weight.cap), None);
        }
    }
    if facts.source_touched_without_tests {
        if let Some(weight) = w.get("noTestsWithSource") {
            if let Some(points) = weight.points {
                push("noTestsWithSource", points, None);
            }
        }
    }
    if analyzer_finding_count > 0 {
        if let Some(weight) = w.get("analyzerDensity") {
            if let Some(per_finding_per_kloc) = weight.per_finding_per_kloc {
                let kloc = (lines_changed(facts) / 1000.0).max(0.001);
                let density = analyzer_finding_count as f64 / kloc;
                push("analyzerDensity", clamp(density * per_finding_per_kloc, weight.cap), Some(format!("{analyzer_finding_count} finding(s)")));
            }
        }
    }

    // `+ 0.0` normalizes the -0.0 that Iterator::sum() produces on an empty
    // list (e.g. a diff with no signals at all) — mathematically identical
    // to 0.0, but "-0.0" reads as a bug in report/terminal output.
    let score: f64 = signals.iter().map(|s| s.points).sum::<f64>() + 0.0;
    (score, signals)
}

pub fn tier_for_score(score: f64, config: &AutoreviewConfig) -> Tier {
    let tiers = &config.triage.tiers;
    if let Some(max) = tiers.quick.max_score {
        if score <= max {
            return Tier::Quick;
        }
    }
    if let Some(max) = tiers.standard.max_score {
        if score <= max {
            return Tier::Standard;
        }
    }
    Tier::Deep
}

fn skill_matches_trigger(skill: &SkillManifest, facts: &DiffFacts, signal_names: &[String], tier: Tier) -> (bool, Vec<String>) {
    let mut via = Vec::new();
    if skill.triggers.always {
        via.push("always".to_string());
    }
    for glob_pattern in &skill.triggers.globs {
        if let Ok(glob) = Glob::new(glob_pattern) {
            let matcher = glob.compile_matcher();
            if facts.files.iter().any(|f| matcher.is_match(&f.path)) {
                via.push(format!("glob:{glob_pattern}"));
            }
        }
    }
    for signal in &skill.triggers.signals {
        if signal_names.contains(signal) {
            via.push(format!("signal:{signal}"));
        }
    }

    // Cost-class gating: expensive skills only run at standard/deep unless explicitly triggered
    // by a strong signal (sensitive path, dependency change) that should summon them even in quick tier.
    let strong_override = via.iter().any(|v| v == "signal:sensitivePathHit" || v == "signal:dependencyChange");
    if tier == Tier::Quick && skill.cost_class != autoreview_schema::CostClass::Quick && !strong_override {
        return (false, Vec::new());
    }
    (!via.is_empty(), via)
}

#[derive(Debug, Default, Clone)]
pub struct PlanOverrides {
    pub tier: Option<Tier>,
    pub aspects: Option<Vec<String>>,
    pub max_usd: Option<f64>,
}

pub fn plan_review(facts: &DiffFacts, config: &AutoreviewConfig, skills: &[SkillManifest], analyzer_finding_count: usize, overrides: PlanOverrides) -> ReviewPlan {
    let (score, signals) = score_diff_facts(facts, config, analyzer_finding_count);
    let tier = overrides.tier.unwrap_or_else(|| tier_for_score(score, config));
    let signal_names: Vec<String> = signals.iter().map(|s| s.signal.clone()).collect();

    let budget = match tier {
        Tier::Quick => &config.budgets.tiers.quick,
        Tier::Standard => &config.budgets.tiers.standard,
        Tier::Deep => &config.budgets.tiers.deep,
    };
    let model_alias = budget.per_agent.model;
    let model = match model_alias {
        autoreview_schema::ModelAlias::Cheap => config.budgets.models.cheap.clone(),
        autoreview_schema::ModelAlias::Standard => config.budgets.models.standard.clone(),
        autoreview_schema::ModelAlias::Deep => config.budgets.models.deep.clone(),
    };

    let mut specialists: Vec<SpecialistPlanEntry> = skills
        .iter()
        .filter_map(|skill| {
            let (matched, via) = skill_matches_trigger(skill, facts, &signal_names, tier);
            matched.then(|| SpecialistPlanEntry {
                aspect: skill.id.clone(),
                triggered_by: via,
                model: model.clone(),
                max_turns: budget.per_agent.max_turns,
            })
        })
        .collect();

    if let Some(allow) = &overrides.aspects {
        specialists.retain(|s| allow.contains(&s.aspect));
    }

    specialists.truncate(budget.max_agents as usize);

    let mut override_labels = Vec::new();
    if let Some(t) = overrides.tier {
        override_labels.push(format!("--tier={t}"));
    }
    if let Some(aspects) = &overrides.aspects {
        override_labels.push(format!("--aspects={}", aspects.join(",")));
    }
    if let Some(max_usd) = overrides.max_usd {
        override_labels.push(format!("--max-usd={max_usd}"));
    }

    ReviewPlan {
        tier,
        score,
        signals,
        specialists,
        budgets: PlanBudgets {
            max_agents: budget.max_agents,
            total_token_cap: budget.total_token_cap,
            wall_clock_sec: budget.wall_clock_sec,
        },
        overrides: override_labels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{CostClass, SkillTools, SkillTriggers};
    use std::collections::HashMap;

    fn make_facts(overrides: impl FnOnce(&mut DiffFacts)) -> DiffFacts {
        let mut facts = DiffFacts {
            repo_root: "/repo".into(),
            base_ref: "main~1".into(),
            head_ref: "main".into(),
            files: vec![super::super::signals::FileChange { path: "src/main.go".into(), additions: 5, deletions: 0 }],
            languages: HashMap::from([("go".to_string(), 1)]),
            sensitive_path_hit: false,
            sensitive_paths: vec![],
            dependency_change: false,
            ci_or_infra_change: false,
            tests_touched: false,
            source_touched_without_tests: true,
            added_branch_keywords: 0,
        };
        overrides(&mut facts);
        facts
    }

    fn make_skill(id: &str, cost_class: CostClass, triggers: SkillTriggers) -> SkillManifest {
        SkillManifest {
            id: id.into(),
            title: format!("{id} review"),
            version: "0.1.0".into(),
            categories: vec![id.into()],
            languages: vec!["*".into()],
            triggers,
            cost_class,
            tools: SkillTools::default(),
            output_contract: "findings-json-v1".into(),
        }
    }

    #[test]
    fn empty_diff_scores_as_positive_zero_not_negative_zero() {
        // Regression test: Iterator::sum() over an empty list of signals
        // produces -0.0 in Rust, which is mathematically fine but renders as
        // "-0.0" in terminal/report output and reads as a bug.
        let config = AutoreviewConfig::default();
        let facts = make_facts(|f| {
            f.files = vec![];
            f.source_touched_without_tests = false;
        });
        let (score, signals) = score_diff_facts(&facts, &config, 0);
        assert!(signals.is_empty());
        assert!(!score.is_sign_negative(), "score should be +0.0, not -0.0");
    }

    #[test]
    fn scores_a_tiny_uneventful_diff_into_quick_tier() {
        let config = AutoreviewConfig::default();
        let facts = make_facts(|f| {
            f.files = vec![super::super::signals::FileChange { path: "src/util.go".into(), additions: 2, deletions: 1 }];
            f.source_touched_without_tests = false;
        });
        let (score, _) = score_diff_facts(&facts, &config, 0);
        assert_eq!(tier_for_score(score, &config), Tier::Quick);
    }

    #[test]
    fn escalates_tier_when_sensitive_path_touched() {
        let config = AutoreviewConfig::default();
        let facts = make_facts(|f| {
            f.sensitive_path_hit = true;
            f.sensitive_paths = vec!["auth/login.go".into()];
        });
        let (score, signals) = score_diff_facts(&facts, &config, 0);
        assert!(signals.iter().any(|s| s.signal == "sensitivePathHit"));
        assert_ne!(tier_for_score(score, &config), Tier::Quick);
    }

    #[test]
    fn respects_configured_per_signal_point_cap() {
        let config = AutoreviewConfig::default();
        let facts = make_facts(|f| {
            f.files = (0..500).map(|i| super::super::signals::FileChange { path: format!("f{i}.go"), additions: 1, deletions: 0 }).collect();
        });
        let (_, signals) = score_diff_facts(&facts, &config, 0);
        let files_changed = signals.iter().find(|s| s.signal == "filesChanged").unwrap();
        let cap = config.triage.signals.get("filesChanged").unwrap().cap.unwrap();
        assert!(files_changed.points <= cap);
    }

    #[test]
    fn analyzer_findings_contribute_to_the_score_and_are_capped() {
        let config = AutoreviewConfig::default();
        let facts = make_facts(|_| {});

        let (baseline_score, baseline_signals) = score_diff_facts(&facts, &config, 0);
        assert!(!baseline_signals.iter().any(|s| s.signal == "analyzerDensity"));

        let (with_findings_score, with_findings_signals) = score_diff_facts(&facts, &config, 3);
        let density_signal = with_findings_signals.iter().find(|s| s.signal == "analyzerDensity").unwrap();
        let cap = config.triage.signals.get("analyzerDensity").unwrap().cap.unwrap();
        assert!(density_signal.points <= cap);
        assert!(with_findings_score > baseline_score);
    }

    #[test]
    fn does_not_summon_expensive_skill_in_quick_tier_without_strong_trigger() {
        let config = AutoreviewConfig::default();
        let facts = make_facts(|_| {});
        let skills = vec![make_skill("security", CostClass::Expensive, SkillTriggers { globs: vec![], signals: vec![], always: true })];
        let plan = plan_review(&facts, &config, &skills, 0, PlanOverrides::default());
        assert_eq!(plan.tier, Tier::Quick);
        assert!(!plan.specialists.iter().any(|s| s.aspect == "security"));
    }

    #[test]
    fn summons_expensive_skill_in_quick_tier_when_triggered_by_strong_signal() {
        let mut config = AutoreviewConfig::default();
        // Force the tier to stay quick despite the sensitive-path hit, so this test
        // isolates the "strong trigger overrides cost-class gating" behavior itself.
        config.triage.signals.get_mut("sensitivePathHit").unwrap().points = Some(5.0);
        config.triage.tiers.quick.max_score = Some(50.0);

        let facts = make_facts(|f| {
            f.sensitive_path_hit = true;
            f.sensitive_paths = vec!["auth/login.go".into()];
        });
        let skills = vec![make_skill("security", CostClass::Expensive, SkillTriggers { globs: vec![], signals: vec!["sensitivePathHit".into()], always: false })];
        let plan = plan_review(&facts, &config, &skills, 0, PlanOverrides::default());

        assert_eq!(plan.tier, Tier::Quick);
        assert!(plan.specialists.iter().any(|s| s.aspect == "security"));
        assert!(plan.specialists.iter().find(|s| s.aspect == "security").unwrap().triggered_by.contains(&"signal:sensitivePathHit".to_string()));
    }

    #[test]
    fn caps_specialists_at_tier_max_agents_budget() {
        let mut config = AutoreviewConfig::default();
        config.budgets.tiers.deep.max_agents = 2;
        let facts = make_facts(|_| {});
        let skills: Vec<SkillManifest> = ["a", "b", "c", "d"]
            .iter()
            .map(|id| make_skill(id, CostClass::Moderate, SkillTriggers { globs: vec![], signals: vec![], always: true }))
            .collect();
        let plan = plan_review(&facts, &config, &skills, 0, PlanOverrides { tier: Some(Tier::Deep), ..Default::default() });
        assert_eq!(plan.specialists.len(), 2);
    }

    #[test]
    fn applies_aspects_override_to_filter_specialists() {
        let config = AutoreviewConfig::default();
        let facts = make_facts(|_| {});
        let skills: Vec<SkillManifest> = ["security", "design", "style"]
            .iter()
            .map(|id| make_skill(id, CostClass::Moderate, SkillTriggers { globs: vec![], signals: vec![], always: true }))
            .collect();
        let plan = plan_review(&facts, &config, &skills, 0, PlanOverrides { tier: Some(Tier::Deep), aspects: Some(vec!["security".into()]), ..Default::default() });
        assert_eq!(plan.specialists.iter().map(|s| s.aspect.clone()).collect::<Vec<_>>(), vec!["security".to_string()]);
    }

    #[test]
    fn records_overrides_for_report_transparency() {
        let config = AutoreviewConfig::default();
        let facts = make_facts(|_| {});
        let plan = plan_review(&facts, &config, &[], 0, PlanOverrides { tier: Some(Tier::Deep), max_usd: Some(2.0), ..Default::default() });
        assert!(plan.overrides.contains(&"--tier=deep".to_string()));
        assert!(plan.overrides.contains(&"--max-usd=2".to_string()));
    }
}
