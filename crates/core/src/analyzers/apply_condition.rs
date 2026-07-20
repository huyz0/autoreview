//! A deterministic, cheap-to-evaluate predicate gating whether a rule
//! group or analyzer can possibly fire on the current diff — evaluated
//! once against facts already computed for the diff (which languages are
//! present), never against a per-rule cost (subprocess spawn, parse,
//! whole-repo build). See the "Rule groups + deterministic apply
//! conditions" plan for the full design.

use std::collections::HashSet;

use autoreview_langsupport::Language;

pub enum ApplyCondition {
    /// Primary axis: applies iff the diff touches at least one file in one
    /// of these languages — what nearly every rule/group actually needs.
    AnyLanguage(&'static [Language]),
    /// Fallback axis for a rule/group not tied to a single language, whose
    /// entire output falls into a fixed, known set of categories (e.g.
    /// `complexity.rs`'s findings are always `category: "design"`).
    /// Applies unconditionally when the caller has no category
    /// restriction to check against (`categories_present: None` — the
    /// common case, no `--aspects` given); when the caller passes a
    /// restricted set (`--aspects` was given), applies only if at least
    /// one of this condition's categories is in it.
    AnyCategory(&'static [&'static str]),
    /// No gate at all.
    Always,
}

impl ApplyCondition {
    pub fn applies(&self, languages_present: &HashSet<Language>, categories_present: Option<&HashSet<String>>) -> bool {
        match self {
            ApplyCondition::AnyLanguage(langs) => langs.iter().any(|l| languages_present.contains(l)),
            ApplyCondition::AnyCategory(cats) => match categories_present {
                None => true,
                Some(present) => cats.iter().any(|c| present.contains(*c)),
            },
            ApplyCondition::Always => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_language_applies_when_one_of_its_languages_is_present() {
        let present: HashSet<Language> = [Language::Go].into_iter().collect();
        assert!(ApplyCondition::AnyLanguage(&[Language::Go, Language::Java]).applies(&present, None));
    }

    #[test]
    fn any_language_does_not_apply_when_none_of_its_languages_are_present() {
        let present: HashSet<Language> = [Language::Kotlin].into_iter().collect();
        assert!(!ApplyCondition::AnyLanguage(&[Language::Go, Language::Java]).applies(&present, None));
    }

    #[test]
    fn any_language_does_not_apply_to_an_empty_diff() {
        assert!(!ApplyCondition::AnyLanguage(&[Language::Go]).applies(&HashSet::new(), None));
    }

    #[test]
    fn any_category_applies_unconditionally_when_no_category_restriction_is_given() {
        assert!(ApplyCondition::AnyCategory(&["design"]).applies(&HashSet::new(), None));
    }

    #[test]
    fn any_category_applies_when_one_of_its_categories_is_in_the_restricted_set() {
        let present: HashSet<String> = ["design".to_string(), "security".to_string()].into_iter().collect();
        assert!(ApplyCondition::AnyCategory(&["design"]).applies(&HashSet::new(), Some(&present)));
    }

    #[test]
    fn any_category_does_not_apply_when_none_of_its_categories_are_in_the_restricted_set() {
        let present: HashSet<String> = ["security".to_string()].into_iter().collect();
        assert!(!ApplyCondition::AnyCategory(&["design"]).applies(&HashSet::new(), Some(&present)));
    }

    #[test]
    fn always_applies_regardless_of_inputs() {
        assert!(ApplyCondition::Always.applies(&HashSet::new(), None));
        let present: HashSet<String> = ["security".to_string()].into_iter().collect();
        assert!(ApplyCondition::Always.applies(&HashSet::new(), Some(&present)));
    }
}
