//! Mines candidate rule seeds from recurring **human Bitbucket PR review
//! comments** — the Bitbucket-side sibling of `mine_from_comments.rs`
//! (GitHub), structurally identical (same `MinedFindingRow` shape, same
//! `mine_candidates` clustering downstream) but hitting Bitbucket Cloud's
//! REST API via curl + a stored `CredentialStore` token instead of
//! shelling out to `gh`.
//!
//! Response shapes below (`BbPage`, `BbPullRequest`, `BbComment`) are
//! modeled directly off real API responses fetched from a real public
//! Bitbucket repository (`atlassian/atlassian-plugins`) during
//! development, not guessed from documentation — Bitbucket Cloud's list
//! endpoints wrap results in `{values: [...], next: "<url>"|null, ...}`,
//! a comment's text lives at `content.raw`, and a merged PR's id is a
//! plain integer at `.id`.
//!
//! One real difference from the GitHub source, stated plainly: Bitbucket
//! comment authors have no `type: "Bot"` field the way GitHub's do, so
//! there's no equivalent bot-filtering here — every non-deleted,
//! long-enough comment is mined regardless of who posted it.
//!
//! `run_id` is `"bb-pr-<id>"`, mirroring GitHub's `"pr-<number>"` — the
//! same ">= 2 distinct PRs" recurrence signal, just namespaced so a
//! GitHub PR #42 and a Bitbucket PR #42 mined in the same run never
//! collide.

use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::rule_factory::category_heuristics::guess_category;
use crate::storage::history_store::MinedFindingRow;

const MIN_COMMENT_LEN: usize = 25;
const TITLE_PREFIX_CHARS: usize = 80;
/// Bounds how many pages of pagination this will follow for either the
/// PR list or one PR's comments — a safety net against an unbounded
/// `next`-link chain on a huge, ancient repository, the same purpose
/// `mine_from_bugfix_commits::DEFAULT_MAX_COMMITS` serves for its own
/// source.
const MAX_PAGES: usize = 10;

#[derive(Debug, Deserialize)]
struct BbPage<T> {
    values: Vec<T>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BbPullRequest {
    id: u64,
}

#[derive(Debug, Default, Deserialize)]
struct BbContent {
    #[serde(default)]
    raw: String,
}

#[derive(Debug, Deserialize)]
struct BbComment {
    id: u64,
    #[serde(default)]
    content: BbContent,
    #[serde(default)]
    deleted: bool,
}

/// Empty `email`/`token` deliberately sends no credential at all rather
/// than an empty one — confirmed empirically against a real public
/// repository that Bitbucket treats an explicit-but-empty Basic auth
/// header as a real (failing) authentication attempt, not the same thing
/// as sending no `Authorization` header at all, so an empty credential
/// gets a real 401 even for anonymous-readable public repos.
///
/// When there *is* a credential it travels in a `0600` curl config file
/// (`auth::curl_config`), never as a `-u` argv entry that `ps` would
/// expose to every other user on the machine.
fn run_curl_json(url: &str, email: &str, token: &str, curl_binary: &str) -> anyhow::Result<String> {
    let auth_config = if email.is_empty() && token.is_empty() {
        None
    } else {
        Some(crate::auth::curl_config::CurlAuthConfig::basic(email, token).map_err(|err| anyhow::anyhow!("failed to stage curl credentials: {err}"))?)
    };
    let config_path = auth_config.as_ref().map(|c| c.path().display().to_string());

    let mut args = vec!["-sS"];
    if let Some(path) = &config_path {
        args.extend(["--config", path]);
    }
    args.extend(["-w", "\n%{http_code}", "--max-time", "20", url]);
    // `curl_binary`, not a hardcoded "curl": `mineFromBitbucketComments.
    // curlBinary` is a real setting, and it previously appeared only in
    // this function's error message while the actual invocation ignored
    // it — so configuring it did nothing.
    let output = Command::new(curl_binary).args(&args).output().map_err(|err| anyhow::anyhow!("failed to reach Bitbucket ({curl_binary} error): {err}"))?;
    if !output.status.success() {
        anyhow::bail!("curl failed to run: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut lines: Vec<&str> = stdout.lines().collect();
    let status_line = lines.pop().unwrap_or_default();
    let status: u32 = status_line.trim().parse().unwrap_or(0);
    let body = lines.join("\n");
    if status != 200 {
        anyhow::bail!("Bitbucket returned HTTP {status} for {url}");
    }
    Ok(body)
}

fn list_merged_pr_ids(email: &str, token: &str, workspace: &str, repo_slug: &str, limit: usize, curl_binary: &str) -> anyhow::Result<Vec<u64>> {
    let pagelen = limit.clamp(1, 50);
    let mut next = Some(format!("https://api.bitbucket.org/2.0/repositories/{workspace}/{repo_slug}/pullrequests?state=MERGED&sort=-created_on&pagelen={pagelen}"));
    let mut ids = Vec::new();
    let mut pages = 0;
    while let Some(url) = next {
        if pages >= MAX_PAGES || ids.len() >= limit {
            break;
        }
        let body = run_curl_json(&url, email, token, curl_binary)?;
        let page: BbPage<BbPullRequest> = serde_json::from_str(&body)?;
        ids.extend(page.values.iter().map(|pr| pr.id));
        next = page.next;
        pages += 1;
    }
    ids.truncate(limit);
    Ok(ids)
}

fn fetch_pr_comments(email: &str, token: &str, workspace: &str, repo_slug: &str, pr_id: u64, curl_binary: &str) -> anyhow::Result<Vec<BbComment>> {
    let mut next = Some(format!("https://api.bitbucket.org/2.0/repositories/{workspace}/{repo_slug}/pullrequests/{pr_id}/comments?pagelen=50"));
    let mut comments = Vec::new();
    let mut pages = 0;
    while let Some(url) = next {
        if pages >= MAX_PAGES {
            break;
        }
        let body = run_curl_json(&url, email, token, curl_binary)?;
        let page: BbPage<BbComment> = serde_json::from_str(&body)?;
        comments.extend(page.values);
        next = page.next;
        pages += 1;
    }
    Ok(comments)
}

fn comment_to_mined_row(comment: &BbComment, pr_id: u64) -> Option<MinedFindingRow> {
    if comment.deleted {
        return None;
    }
    let body = comment.content.raw.trim();
    if body.chars().count() < MIN_COMMENT_LEN {
        return None;
    }
    let title: String = body.chars().take(TITLE_PREFIX_CHARS).collect();
    Some(MinedFindingRow {
        fingerprint: format!("bb-comment-{}", comment.id),
        category: guess_category(body),
        rule_id_or_aspect: "pr-review-comment".to_string(),
        title,
        message: body.to_string(),
        run_id: format!("bb-pr-{pr_id}"),
    })
}

/// Extracts `(workspace, repo_slug)` from `origin`'s remote URL —
/// handles both the SSH (`git@bitbucket.org:workspace/repo.git`) and
/// HTTPS (`https://bitbucket.org/workspace/repo.git`) forms Bitbucket
/// issues. `None` for a remote that isn't a `bitbucket.org` URL at all,
/// or one missing either path segment.
pub fn resolve_bitbucket_repo_slug(repo_root: &Path) -> Option<(String, String)> {
    let output = Command::new("git").args(["remote", "get-url", "origin"]).current_dir(repo_root).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_bitbucket_workspace_and_slug(&url)
}

fn parse_bitbucket_workspace_and_slug(remote_url: &str) -> Option<(String, String)> {
    let path = if let Some(rest) = remote_url.strip_prefix("git@bitbucket.org:") {
        rest
    } else {
        let idx = remote_url.find("bitbucket.org/")?;
        &remote_url[idx + "bitbucket.org/".len()..]
    };
    let path = path.trim_end_matches(".git").trim_end_matches('/');
    let mut parts = path.splitn(2, '/');
    let workspace = parts.next()?.to_string();
    let repo_slug = parts.next()?.to_string();
    if workspace.is_empty() || repo_slug.is_empty() {
        return None;
    }
    Some((workspace, repo_slug))
}

/// Mines the `lookback_prs` most recently merged PRs' comments from one
/// Bitbucket Cloud repository into `MinedFindingRow`s, ready for
/// `mine::mine_candidates`. Real I/O — one PR-list call (paginated) plus
/// one comments call (also paginated) per PR, sequential — same "not
/// worth a worker pool for an occasional, opt-in operation, and
/// concurrent requests risk secondary rate limits" reasoning
/// `mine_from_comments.rs` already states for GitHub.
pub fn mine_from_bitbucket_pr_comments(email: &str, token: &str, workspace: &str, repo_slug: &str, lookback_prs: usize, curl_binary: &str) -> anyhow::Result<Vec<MinedFindingRow>> {
    let pr_ids = list_merged_pr_ids(email, token, workspace, repo_slug, lookback_prs, curl_binary)?;
    let mut rows = Vec::new();
    for pr_id in pr_ids {
        let Ok(comments) = fetch_pr_comments(email, token, workspace, repo_slug, pr_id, curl_binary) else { continue };
        rows.extend(comments.iter().filter_map(|c| comment_to_mined_row(c, pr_id)));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_shaped_pr_list_page() {
        // Captured from a real public repo:
        // GET /2.0/repositories/atlassian/atlassian-plugins/pullrequests?state=MERGED
        let body = r#"{"values":[{"id":447,"title":"PLUG-1239: use synchronous Gemini shutdown by default","state":"MERGED"}],"pagelen":1,"size":866,"page":1,"next":"https://api.bitbucket.org/2.0/repositories/atlassian/atlassian-plugins/pullrequests?state=MERGED&page=2"}"#;
        let page: BbPage<BbPullRequest> = serde_json::from_str(body).unwrap();
        assert_eq!(page.values[0].id, 447);
        assert!(page.next.is_some());
    }

    #[test]
    fn parses_a_real_shaped_comment_and_maps_it_to_a_mined_row() {
        // Captured from a real public repo:
        // GET /2.0/repositories/atlassian/atlassian-plugins/pullrequests/447/comments
        let body = r#"{"id":112839025,"created_on":"2019-08-13T00:32:21.802130+00:00","content":{"type":"rendered","raw":"Thanks for the contribution. What do the product teams think about this change? Is there a use case for shutting down asynchronously, given that the speed is about the same and it creates the risk of NPEs?","markup":"markdown","html":"<p>...</p>"},"user":{"display_name":"Andrew S","type":"user"},"deleted":false,"pending":false,"type":"pullrequest_comment"}"#;
        let comment: BbComment = serde_json::from_str(body).unwrap();
        let row = comment_to_mined_row(&comment, 447).unwrap();
        assert_eq!(row.fingerprint, "bb-comment-112839025");
        assert_eq!(row.run_id, "bb-pr-447");
        assert_eq!(row.rule_id_or_aspect, "pr-review-comment");
        assert!(row.title.starts_with("Thanks for the contribution"), "got: {}", row.title);
    }

    #[test]
    fn filters_out_a_deleted_comment() {
        let comment = BbComment { id: 1, content: BbContent { raw: "a".repeat(50) }, deleted: true };
        assert!(comment_to_mined_row(&comment, 1).is_none());
    }

    #[test]
    fn filters_out_short_low_signal_comments() {
        for body in ["lgtm", "nit", "+1", ""] {
            let comment = BbComment { id: 1, content: BbContent { raw: body.to_string() }, deleted: false };
            assert!(comment_to_mined_row(&comment, 1).is_none(), "expected {body:?} to be filtered");
        }
    }

    #[test]
    fn parses_workspace_and_repo_slug_from_an_https_remote() {
        assert_eq!(parse_bitbucket_workspace_and_slug("https://bitbucket.org/my-team/my-repo.git"), Some(("my-team".to_string(), "my-repo".to_string())));
    }

    #[test]
    fn parses_workspace_and_repo_slug_from_an_ssh_remote() {
        assert_eq!(parse_bitbucket_workspace_and_slug("git@bitbucket.org:my-team/my-repo.git"), Some(("my-team".to_string(), "my-repo".to_string())));
    }

    #[test]
    fn returns_none_for_a_non_bitbucket_remote() {
        assert_eq!(parse_bitbucket_workspace_and_slug("git@github.com:my-team/my-repo.git"), None);
    }

    #[test]
    fn different_prs_yield_distinct_run_ids() {
        let a = comment_to_mined_row(&BbComment { id: 1, content: BbContent { raw: "Missing null check before dereferencing this pointer here.".to_string() }, deleted: false }, 100).unwrap();
        let b = comment_to_mined_row(&BbComment { id: 2, content: BbContent { raw: "Missing null check before dereferencing this pointer here.".to_string() }, deleted: false }, 200).unwrap();
        assert_ne!(a.run_id, b.run_id);
    }

    #[test]
    fn mines_real_pr_comments_from_a_real_public_bitbucket_repository() {
        // Real network call, no credential needed — Bitbucket Cloud
        // allows anonymous GET on public repos. Best-effort: skipped,
        // not failed, with no network access, or a real HTTP 429 — this
        // anonymous, unauthenticated endpoint is rate-limited more
        // tightly than an authenticated one, and this exact test hitting
        // it repeatedly during development is itself a real example of
        // that; verifying this module's real-API integration isn't the
        // same claim as verifying it never gets rate-limited.
        match mine_from_bitbucket_pr_comments("", "", "atlassian", "atlassian-plugins", 3, "curl") {
            Ok(rows) => assert!(!rows.is_empty(), "expected at least one substantive comment from real merged PRs"),
            Err(err) if err.to_string().contains("failed to reach Bitbucket") || err.to_string().contains("HTTP 429") => {
                eprintln!("skipping: network unavailable or rate-limited in this environment ({err})");
            }
            Err(err) => panic!("got an unexpected error: {err}"),
        }
    }
}
