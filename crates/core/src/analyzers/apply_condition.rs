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
    /// Fallback axis, reserved for a rule/group not tied to a single
    /// language (e.g. a future cross-language semantic/LLM rule). No
    /// concrete consumer exists yet — always applies until one does; kept
    /// as a documented variant so future code has somewhere to attach
    /// rather than inventing a parallel mechanism, not as machinery
    /// anything currently exercises.
    AnyCategory(&'static [&'static str]),
    /// No gate at all.
    Always,
}

impl ApplyCondition {
    pub fn applies(&self, languages_present: &HashSet<Language>) -> bool {
        match self {
            ApplyCondition::AnyLanguage(langs) => langs.iter().any(|l| languages_present.contains(l)),
            ApplyCondition::AnyCategory(_) => true,
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
        assert!(ApplyCondition::AnyLanguage(&[Language::Go, Language::Java]).applies(&present));
    }

    #[test]
    fn any_language_does_not_apply_when_none_of_its_languages_are_present() {
        let present: HashSet<Language> = [Language::Kotlin].into_iter().collect();
        assert!(!ApplyCondition::AnyLanguage(&[Language::Go, Language::Java]).applies(&present));
    }

    #[test]
    fn any_language_does_not_apply_to_an_empty_diff() {
        assert!(!ApplyCondition::AnyLanguage(&[Language::Go]).applies(&HashSet::new()));
    }

    #[test]
    fn any_category_always_applies_regardless_of_languages_present() {
        assert!(ApplyCondition::AnyCategory(&["security"]).applies(&HashSet::new()));
    }

    #[test]
    fn always_applies_regardless_of_languages_present() {
        assert!(ApplyCondition::Always.applies(&HashSet::new()));
    }
}
