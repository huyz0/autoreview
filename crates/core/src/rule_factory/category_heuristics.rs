//! Shared free-text category guessing for mining sources whose raw input
//! (a PR comment, a bug-fix commit message) has no reliable structured
//! category of its own — extracted out of `mine_from_comments.rs`
//! (originally its sole caller) so `mine_from_bugfix_commits.rs` can reuse
//! the exact same heuristic instead of drifting into a second, subtly
//! different keyword list.

/// Lightweight keyword bucketing, not a claim of accuracy — a human always
/// reviews a candidate at the `rules review --approve` gate before it ever
/// reaches shadow mode, so an imperfect initial category guess costs
/// nothing but a slightly-off grouping label, not a wrongly-shipped rule.
/// Checked in a fixed, deliberately narrow order (security first, since a
/// comment mentioning both "sql injection" and "slow" should land under
/// the security bucket, not performance) — first match wins.
pub fn guess_category(text: &str) -> String {
    let lower = text.to_lowercase();
    const SECURITY_KEYWORDS: &[&str] = &["inject", "vulnerab", "secret", "credential", "xss", "csrf", "sanitiz", "escape", "auth", "password", "token"];
    const PERFORMANCE_KEYWORDS: &[&str] = &["slow", "performance", "n+1", "allocat", "loop", "o(n", "cache", "latency"];
    const DESIGN_KEYWORDS: &[&str] = &["naming", "extract", "duplicat", "readab", "abstraction", "coupling", "responsibility"];
    if SECURITY_KEYWORDS.iter().any(|k| lower.contains(k)) {
        "security".to_string()
    } else if PERFORMANCE_KEYWORDS.iter().any(|k| lower.contains(k)) {
        "performance".to_string()
    } else if DESIGN_KEYWORDS.iter().any(|k| lower.contains(k)) {
        "design".to_string()
    } else {
        "correctness".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_security_category_for_injection_language() {
        assert_eq!(guess_category("This looks vulnerable to SQL injection if the input isn't sanitized."), "security");
    }

    #[test]
    fn guesses_performance_category_for_n_plus_one_language() {
        assert_eq!(guess_category("This issues a query inside the loop, which is an N+1 pattern."), "performance");
    }

    #[test]
    fn security_keywords_win_over_performance_keywords_when_both_present() {
        assert_eq!(guess_category("This SQL injection risk also makes the loop slow."), "security");
    }

    #[test]
    fn defaults_to_correctness_when_no_keyword_matches() {
        assert_eq!(guess_category("This should check for nil before dereferencing."), "correctness");
    }
}
