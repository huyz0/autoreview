---
name: running-reviews
description: >
  Runs an autoreview code review over a diff and acts on what it finds —
  interpreting the report, explaining why a finding fired, applying a
  suggested patch, and recording verdicts that feed the rule-learning loop.
  Use when asked to "review this branch/PR/diff", "run autoreview", "what
  does autoreview say about X", "check this change before I push", "apply
  that suggestion", or "mark that finding a false positive". Covers the
  diff/explain/apply/feedback/doctor commands, the report layout, cost and
  tier controls, and the feedback taxonomy an agent must get right.
---

autoreview reviews a git diff in stages: deterministic analyzers first
(ast-grep rules, golangci-lint, clippy, complexity, duplication,
architecture, dataflow), then — depending on a triage score — LLM
specialists for what static rules can't judge. Reports land in a cache
directory, and every finding gets a stable id you can explain, apply, or
give a verdict on.

To author a *new rule* rather than run a review, use the
[`authoring-rules`](../authoring-rules/SKILL.md) skill instead.

## Always check coverage first

```bash
autoreview doctor
```

**A missing analyzer degrades the review silently.** Verified: the same
Go diff containing a self-comparison and a shell-injection concat
reported `stage 1: 2 deterministic finding(s)` with `ast-grep` on PATH,
and `stage 1: 0 deterministic finding(s)` without it — no warning either
time, and the triage tier quietly dropped from `standard` to `quick` as a
result. **Never report "autoreview found nothing" as "the code is clean"
without having checked `doctor` first.** Say which analyzers were
unavailable and what that leaves unchecked.

`doctor` also reports which agent backend is available. Only the one
selected by `agents.backend` (or `--backend`) needs to be there; the rest
are informational.

## Run the review

```bash
autoreview diff --base origin/main --head HEAD
```

Defaults are `--base origin/main --head HEAD`, so a bare `autoreview
diff` reviews the current branch. Flags that matter:

| Flag | Use it when |
|---|---|
| `--max-usd <n>` | **Always set this for an unattended run.** Specialists cost real money; without a cap the only limits are the tier's token/agent budgets. |
| `--tier quick\|standard\|deep` | Override the computed triage tier. `quick` skips the verify pass entirely. |
| `--aspects security,correctness` | Restrict which specialists run. Cheaper and faster when the question is narrow. |
| `--incremental` | Suppress findings already reported in the previous run on this repo — good for re-reviewing after a fixup commit. |
| `--backend` | `claude-code` (default), `pi`, `local-llm`, `openai-compatible`. |
| `--watch` | Re-run whenever `base...head` changes. Interactive use only, never in CI. |

The run prints its triage score, the tier it picked, the budget, and
where it wrote each report. Two budgets are enforced during dispatch: the
`--max-usd` cap and the tier's `wallClockSec`. Both stop *starting* new
specialists; work already in flight finishes.

## Read the report

The run prints four paths under
`~/.cache/autoreview/<repoFingerprint>/runs/<runId>/`:

- `report.json` — the machine-readable one; parse this
- `report.md` — human-readable summary
- `report.sarif` — for CI/code-scanning upload
- `index.md` — findings grouped by category and by path

`report.json` top-level keys: `schemaVersion`, `runId`, `createdAt`,
`target`, `plan`, `findings`, `suppressed`, `costs`, `summary`.

Each finding carries an `id` shaped `f-<16 hex chars>`, plus `severity`,
`category`, `title`, `message`, `location` (`path`, `range.startLine`,
`snippet`), and `source` (`tool`, `ruleId`). Read `suppressed`
separately — shadow-mode rules and incremental-mode repeats land there
deliberately, and they are not part of the review's verdict.

## Explain before you act

```bash
autoreview explain f-4ef61ae0720e6748
```

Prints the finding, whether it came from a deterministic rule or an LLM
specialist, and for a deterministic one the **exact rule YAML that
matched**, including its CWE/OWASP metadata. Use it whenever you are
about to tell a human a finding is wrong: it distinguishes "the rule is
badly written" from "the rule is right and the code is wrong", and those
lead to opposite actions.

## Apply a suggested fix

```bash
autoreview apply f-4ef61ae0720e6748
```

Only some findings carry a patch; the command says so plainly when one
doesn't. It is gated — `git apply --check` runs first, and a patch that
leaves a Go/Java file syntactically broken is reverted automatically. It
writes to the working tree and does not commit. Review the resulting
diff yourself before committing it; a patch that applies cleanly is not
automatically a patch that is correct.

## Record a verdict — get this right

```bash
autoreview feedback <id> --tp
autoreview feedback <id> --doesnt-apply --note "why"
```

This is the highest-stakes thing an agent does here, because these
verdicts drive the automatic promote/demote gate that decides which
rules keep running.

| Verdict | Means | Effect on the rule |
|---|---|---|
| `--tp` | Real problem, worth fixing | Confirms the rule |
| `--fp` | **The rule itself is wrong here** | Counts as evidence against the rule; enough of these demote it |
| `--doesnt-apply` | Rule is valid, just not relevant to this code | Confirms the rule; does **not** count against it |
| `--accepted-risk` | Real, author accepts it | Confirms the rule |
| `--fix-in-followup` | Real, deferred to another PR | Confirms the rule |
| `--missed <desc>` | Something autoreview should have caught but didn't | Feeds skill mining |

**Do not reach for `--fp` as the generic "not doing this" verdict.** The
middle three all mean "the finding was correct and I'm not acting on it",
and using `--fp` for them teaches the system to delete rules that work.
Reserve `--fp` for when the rule genuinely misfired, and say why in
`--note` — those notes are what `skills mine` later turns into guidance.

When in doubt between `--fp` and `--doesnt-apply`, ask: *would this rule
be wrong to fire on this pattern in any codebase?* Yes → `--fp`. No, it's
just this case → `--doesnt-apply`.

## Reporting back to a human

Lead with severity and what to do, not with counts. For each finding
worth raising: what it is, where (`path:line`), and whether you verified
it by reading the code. Say explicitly when a finding came from an LLM
specialist rather than a deterministic rule — the confidence differs.

State coverage gaps plainly: which analyzers were missing, whether the
run hit a budget stop (the output says so), and whether specialists ran
at all. `[info] No specialists triggered for this diff at tier 'quick'`
means the deterministic layer is the *entire* review.

## Don'ts

- Don't equate zero findings with clean code — check `doctor` and whether
  specialists ran.
- Don't run unattended without `--max-usd`.
- Don't record feedback on findings you haven't actually investigated;
  the verdicts are training signal, not bookkeeping.
- Don't commit an applied patch without reading it.
- Don't use `--watch` in CI or any non-interactive context.
