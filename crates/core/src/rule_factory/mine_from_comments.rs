//! Mines candidate rule seeds from recurring **human PR review comments** —
//! a second input source for `rule_factory::mine`'s clustering, alongside
//! autoreview's own past agent findings. `mine::mine_candidates` is already
//! generic over `MinedFindingRow`, so this module's only job is producing
//! that shape from a different data source: a team's actual GitHub review
//! history, not just what autoreview already caught. Modeled on Aviator
//! Verify's own "mine invariants from recurring PR review comments" input
//! (`ai_generated_pr_comments`) — see SESSION_NOTES.md for the fuller
//! comparison this was distilled from.
//!
//! `run_id` for a comment-derived row is `"pr-<number>"`, not an autoreview
//! run id — `mine_candidates`' existing "recurs across >= 2 distinct runs"
//! gate becomes, for free, "recurs across >= 2 distinct PRs," exactly the
//! recurrence signal that matters for this source (someone repeating
//! themselves within one PR's back-and-forth isn't a pattern; the same
//! comment showing up on unrelated PRs is).
//!
//! I/O (the `gh` subprocess calls) is kept to two small functions
//! (`list_merged_pr_numbers`/`fetch_review_comments`) so the actual mapping
//! logic (`comment_to_mined_row`, `guess_category`) stays pure and testable
//! against literal fixture JSON — same "separate I/O from pure logic"
//! convention as `rule_factory::bench`'s `count_matching_files`.

use std::process::Command;

use serde::Deserialize;

use crate::storage::history_store::MinedFindingRow;

/// Comments shorter than this are almost always "lgtm"/"nit"/"+1"-shaped
/// noise, not a reusable review pattern — filtered out before they ever
/// reach clustering rather than relying on the similarity threshold to
/// separate them out later.
const MIN_COMMENT_LEN: usize = 25;
/// How much of a comment's body becomes the synthetic "title" half of the
/// `title + message` text `mine_candidates` clusters on — matches
/// `RepresentativeSnippet`'s own display use, not a hard data boundary.
const TITLE_PREFIX_CHARS: usize = 80;

#[derive(Debug, Clone, Deserialize)]
struct GhReviewComment {
    id: u64,
    body: String,
    path: Option<String>,
}

/// Lightweight keyword bucketing, not a claim of accuracy — a human always
/// reviews a candidate at the `rules review --approve` gate before it ever
/// reaches shadow mode, so an imperfect initial category guess costs
/// nothing but a slightly-off grouping label, not a wrongly-shipped rule.
/// Checked in a fixed, deliberately narrow order (security first, since a
/// comment mentioning both "sql injection" and "slow" should land under
/// the security bucket, not performance) — first match wins.
fn guess_category(body: &str) -> String {
    let lower = body.to_lowercase();
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

fn comment_to_mined_row(comment: &GhReviewComment, pr_number: u64) -> Option<MinedFindingRow> {
    let body = comment.body.trim();
    if body.chars().count() < MIN_COMMENT_LEN {
        return None;
    }
    let title: String = body.chars().take(TITLE_PREFIX_CHARS).collect();
    let path_suffix = comment.path.as_deref().map(|p| format!(" (in {p})")).unwrap_or_default();
    Some(MinedFindingRow {
        fingerprint: format!("gh-comment-{}", comment.id),
        category: guess_category(body),
        rule_id_or_aspect: "pr-review-comment".to_string(),
        title: format!("{title}{path_suffix}"),
        message: body.to_string(),
        run_id: format!("pr-{pr_number}"),
    })
}

fn run_gh(gh_binary: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(gh_binary).args(args).output()?;
    if !output.status.success() {
        anyhow::bail!("{gh_binary} {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[derive(Debug, Deserialize)]
struct GhPrNumber {
    number: u64,
}

fn list_merged_pr_numbers(gh_binary: &str, limit: usize) -> anyhow::Result<Vec<u64>> {
    let limit_str = limit.to_string();
    let stdout = run_gh(gh_binary, &["pr", "list", "--state", "merged", "--limit", &limit_str, "--json", "number"])?;
    let prs: Vec<GhPrNumber> = serde_json::from_str(&stdout)?;
    Ok(prs.into_iter().map(|p| p.number).collect())
}

/// `--jq` does the field projection and bot-filtering server-side (`gh`
/// evaluates it against the real GitHub API response before ever printing
/// anything), so this only ever pulls the handful of fields actually used —
/// verified against a real public repo's PR review comments before this
/// query shape was committed to, not assumed from API docs.
fn fetch_review_comments(gh_binary: &str, repo_slug: &str, pr_number: u64) -> anyhow::Result<Vec<GhReviewComment>> {
    let path = format!("repos/{repo_slug}/pulls/{pr_number}/comments");
    let stdout = run_gh(gh_binary, &["api", &path, "--jq", r#"[.[] | select(.user.type != "Bot") | {id, body, path}]"#])?;
    Ok(serde_json::from_str(&stdout)?)
}

/// Resolves `owner/repo` for the current directory's GitHub remote — needed
/// since `gh api`'s path-based calls (unlike `gh pr list`) don't infer the
/// repo from cwd on their own.
fn current_repo_slug(gh_binary: &str) -> anyhow::Result<String> {
    let stdout = run_gh(gh_binary, &["repo", "view", "--json", "owner,name", "--jq", ".owner.login + \"/\" + .name"])?;
    Ok(stdout.trim().to_string())
}

/// Mines the `limit_prs` most recently merged PRs' human review comments
/// into `MinedFindingRow`s, ready to hand to `mine::mine_candidates`
/// alongside (or instead of) autoreview's own agent-finding rows. Real I/O
/// — one `gh pr list` call, plus one `gh api .../comments` call *per PR*,
/// sequential, ~2-3s each against a real active repo (measured manually
/// against `cli/cli`; not parallelized — this is an occasional, explicitly
/// opt-in operation, not a hot path, so the added complexity of a worker
/// pool plus GitHub's own secondary rate limits on concurrent requests
/// didn't seem worth it for a first pass) — so this isn't unit-tested
/// directly; `comment_to_mined_row`/`guess_category` carry the tested
/// logic.
pub fn mine_from_pr_comments(gh_binary: &str, limit_prs: usize) -> anyhow::Result<Vec<MinedFindingRow>> {
    let repo_slug = current_repo_slug(gh_binary)?;
    let pr_numbers = list_merged_pr_numbers(gh_binary, limit_prs)?;
    let mut rows = Vec::new();
    for pr_number in pr_numbers {
        let Ok(comments) = fetch_review_comments(gh_binary, &repo_slug, pr_number) else { continue };
        rows.extend(comments.iter().filter_map(|c| comment_to_mined_row(c, pr_number)));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(id: u64, body: &str, path: Option<&str>) -> GhReviewComment {
        GhReviewComment { id, body: body.to_string(), path: path.map(str::to_string) }
    }

    #[test]
    fn maps_a_real_shaped_comment_into_a_mined_row() {
        let c = comment(3569741960, "The help text says it scans all known agent host directories, but the list is incomplete and misleading.", Some("pkg/cmd/skills/update/update.go"));
        let row = comment_to_mined_row(&c, 13864).unwrap();
        assert_eq!(row.fingerprint, "gh-comment-3569741960");
        assert_eq!(row.run_id, "pr-13864");
        assert_eq!(row.rule_id_or_aspect, "pr-review-comment");
        assert!(row.title.contains("update.go"), "got: {}", row.title);
        assert_eq!(row.message, c.body);
    }

    #[test]
    fn filters_out_short_low_signal_comments() {
        for body in ["lgtm", "nit", "+1", "same here", ""] {
            assert!(comment_to_mined_row(&comment(1, body, None), 1).is_none(), "expected {body:?} to be filtered");
        }
    }

    #[test]
    fn keeps_a_substantive_comment_with_no_path() {
        let c = comment(2, "This function doesn't handle the case where the input list is empty, which will panic downstream.", None);
        assert!(comment_to_mined_row(&c, 1).is_some());
    }

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

    #[test]
    fn different_comments_across_different_prs_yield_distinct_run_ids() {
        let a = comment_to_mined_row(&comment(1, "Missing null check before dereferencing this pointer here.", None), 100).unwrap();
        let b = comment_to_mined_row(&comment(2, "Missing null check before dereferencing this pointer here.", None), 200).unwrap();
        assert_ne!(a.run_id, b.run_id);
    }
}
