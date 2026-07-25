---
name: authoring-rules
description: >
  Distills a recurring pattern (a bug-fix commit, a PR review comment thread,
  a repeated code-review note, a codebase convention) into a deterministic
  autoreview rule and gets it running. Use when the user asks to "turn this
  into a rule", "add a lint rule for X", "stop this from happening again",
  "codify this review comment", or wants autoreview to catch a pattern it
  currently misses. Covers the rule DSL (pattern/taint/threshold/call-sequence
  kinds), where to look for evidence, how to avoid duplicating an existing
  rule, and the validate/register workflow via the autoreview CLI.
---

autoreview is a static-review CLI with its own rule engine (ast-grep-based
pattern matching, plus taint/threshold/call-sequence kinds — see
`reference/rule-dsl.md`). This skill turns something a human noticed once
into a rule that catches it automatically from then on. To *run* a review
and act on its findings rather than author a rule, use the
[`running-reviews`](../running-reviews/SKILL.md) skill instead.

It uses whatever tools
you already have (repo access, `git`, `gh`/Bitbucket API, grep) instead of
autoreview's own limited, non-interactive mining passes — you can actually
read the PR discussion, follow the fix across files, and use judgment about
whether a pattern is real, which `autoreview rules mine --from-*` cannot.

## Workflow

1. **Find the evidence.** Don't invent a pattern from a single hunch — find
   at least 2-3 real occurrences, or one clearly-reasoned incident with an
   obvious general case. Where to look:
   - `git log --grep` for bug-fix-shaped commits, then `git show <sha>` /
     `git diff <sha>~1 <sha>` to see what actually changed.
   - PR review comments (`gh pr view <n> --comments`, or the Bitbucket API)
     — a comment repeated across multiple PRs is strong signal.
   - Existing suppression comments in the code (`// nolint`,
     `@SuppressWarnings`, `// eslint-disable`, `# noqa`) — these mark a
     linter rule the team already cares about that autoreview may not cover.
   - A convention you notice while reading the code (e.g. every call to
     `Lock()` is followed by `Unlock()` except one).

2. **Check it's not already covered.** Grep the rule catalog before drafting
   anything — a near-duplicate rule is worse than no rule (noisy, confusing
   which one "owns" a finding):
   ```
   grep -ril "<keyword>" crates/core/rules-builtin/ 2>/dev/null
   ```
   If autoreview isn't vendored in this repo, check whatever rule packs
   *are* registered: read `.autoreview/rulepacks.yaml` and grep each pack's
   directory the same way.

3. **Pick a target for the rule** — this is the one real decision point:

   | Situation | Target |
   |---|---|
   | You're confident this is a real, general pattern (multiple clear occurrences, or a well-reasoned single incident) | A **local rule pack** — durable, active on every review immediately |
   | The evidence is thin, or you want it to prove itself before it can block anyone's review | A **candidate**, through the existing mine → review → shadow pipeline — starts silent, earns trust from real firing history before it ever surfaces |

   Default to the rule pack when you're actually confident; use the
   candidate path when you're not, or when the user explicitly wants review
   before it goes live. Don't skip evidence-gathering (step 1) just because
   the candidate path has a safety net downstream — a rule built on a real
   pattern bench-tests and reviews far better than one built on a guess.

4. **Author the rule.** See `reference/rule-dsl.md` for the exact YAML shape
   per kind, the category taxonomy, and severity levels — don't guess these,
   they're validated exactly by `autoreview rules packs validate`.

   - **Rule pack path**: create (or extend) a local directory with a
     `rulepack.yaml` manifest (`id`, `version`, `description`) and one YAML
     file per rule, organized however the existing packs in this repo are
     (commonly `<language>/<category>/<rule-id>.yml`).
   - **Candidate path**: write straight to
     `.autoreview/rules/candidates/<a-short-kebab-id>/rule.yaml` in the
     target repo — the id doesn't have to come from a real mining run, a
     hand-picked slug describing the pattern is fine.

5. **Validate before trusting your own YAML.**
   - Rule pack: `autoreview rules packs validate <path-to-pack-dir>` —
     checks the manifest, every rule's required fields for its declared
     `kind`, and catches duplicate ids within the pack. Fix everything it
     reports; it will not register something malformed.
   - Candidate: there's no standalone validate command for a single
     candidate — `autoreview rules bench <clusterId>` implicitly requires
     the rule to parse. For a bench self-test to run for real (not report
     "skipped — no fixtures"), add fixture files: at least one match under
     `.autoreview/rules/candidates/<id>/tests/positive/`, at least one
     clean file under `.../tests/negative/`, in the rule's language.

6. **Put it to work.**
   - Rule pack: `autoreview rules packs add <path>` registers it in
     `.autoreview/rulepacks.yaml`. It runs at full trust by default —
     mention to the user they can edit `trust: shadow` there if they'd
     rather it start silent too.
   - Candidate: `autoreview rules bench <clusterId>` to see the verdict,
     then hand off to the user (or run yourself, if asked) —
     `autoreview rules review --approve <clusterId>` moves it to shadow
     mode, where it accumulates real firing history before autoreview's
     own automatic gate promotes it.

## Output contract

When asked to just produce a rule (not run the CLI yourself), give the user:
the rule YAML, which target you'd register it under and why, and the exact
command(s) from steps 5-6 they'd run. Don't claim a rule is validated or
registered unless you actually ran the command and saw it succeed.

## Don'ts

- Don't draft a rule from a single unclear complaint — ask for a second
  example, or fall back to the candidate path so it earns trust over time.
- Don't widen an existing rule to cover a new case if a second, narrower
  rule would be clearer — check what similar rules in this repo already do
  before choosing.
- Don't pick `kind: pattern` for something that genuinely needs dataflow
  (does untrusted input reach a sink?) or a call-ordering fact (was a lock
  released on every path?) — that's what `taint`/`call-sequence` are for.
  A plain pattern can only see local syntax, not either of those.
- Don't forget `semantic: true` on a rule whose match is heuristic rather
  than exactly verified (no real type/dataflow resolution backing it) —
  it's what routes the finding through an automatic LLM confirmation pass
  instead of being trusted outright. See the reference for when it applies.
