# autoreview

A portable, deterministic-first code review CLI. It reviews a git diff the
way a careful senior engineer would: run the cheap, reliable checks first
(ast-grep patterns, dataflow/taint analysis, complexity metrics, import-cycle
detection), and only spend LLM budget on the parts those checks can't cover.

Everything a deterministic analyzer finds is real and cheap to compute.
LLM specialists get called in proportion to how risky the diff actually looks,
scored by a triage step that weighs file count, churn, test coverage, and how
sensitive the touched paths are. A small change to a well-tested utility
function gets a quick pass; a large change to authentication code gets the
deep tier.

## What it checks

- **ast-grep pattern rules** across Go, Java, Kotlin, TypeScript, and
  JavaScript: security (injection, deserialization, weak crypto), correctness
  (self-comparison, empty catch blocks, equals/hashCode contracts), design
  (god classes, generic exception handling), performance.
- **Dataflow and taint tracking** for Go, Java, and Kotlin: a real CFG-based
  engine, not just syntax matching, catches things like a tainted HTTP
  parameter reaching `exec.Command`, or a typed nil pointer silently
  satisfying an `error` interface.
- **Complexity metrics**: cyclomatic/cognitive complexity, long methods, deep
  nesting, god classes, all with YAML-configurable thresholds.
- **Cross-file structure**: import-cycle detection (Go, Java, Kotlin),
  ArchUnit-style layer rules from `.autoreview/architecture.yaml`, symbol-index-backed
  smells like feature envy and data clumps.
- **golangci-lint and clippy** wrapped and normalized into the same finding
  format as everything else.
- **LLM specialists** for the things only a model can judge: does this
  actually do what the PR claims, is this abstraction the right shape, is
  this test meaningful or just padding coverage.

Every finding a rule produces gets deduplicated against your own review
history, so the same rule doesn't nag you about the same code twice, and a
rule with too many false positives on your codebase gets flagged for
tuning instead of silently eroding trust in the tool.

## Install

Not published anywhere yet. Build from source:

```
git clone <this repo>
cd autoreview
cargo build --release
```

The binary lands at `target/release/autoreview`. Put it on your `PATH`, or
run it via `cargo run --release --`.

## Quickstart

Check what's available on your machine:

```
autoreview doctor
```

`git` is required. `ast-grep` and `golangci-lint` are optional but unlock
real coverage for their respective checks (missing tools degrade to "that
check is skipped," never a hard failure). One of `claude` (Claude Code),
`pi`, or a local llama.cpp-compatible server is needed for the LLM
specialist tier, but the deterministic analyzers run fine without any of
them.

Review the current branch against `origin/main`:

```
autoreview diff
```

Or a specific range:

```
autoreview diff --base main~5 --head HEAD
```

Findings print to the terminal and get written as JSON, Markdown, and SARIF
under a machine-local cache directory (the exact path is printed at the end
of the run). Record feedback on a finding to teach the rule factory what's
actually a false positive:

```
autoreview feedback <finding-id> --fp
autoreview feedback <finding-id> --tp
```

## Configuration

Nothing is required. An empty repo with no `.autoreview/` directory at all
runs with sensible defaults. Everything from budget caps to layer rules to
which context files get fed to specialists is opt-in and documented in
[docs/autoreview-directory-layout.md](docs/autoreview-directory-layout.md).

## Extending the rule set

Rules are declarative YAML (`kind: pattern` for ast-grep, `kind: taint` for
dataflow tracking, `kind: threshold` for metrics), not Rust code, so adding
one doesn't mean recompiling. Point autoreview at a shared or third-party
rule pack without forking anything:

```
autoreview rules packs add https://github.com/your-org/rule-pack
autoreview rules packs validate ../rule-pack-in-progress
```

autoreview can also learn rules from your own review history: recurring
patterns in what specialists flag, or recurring human PR comments (opt-in,
via `gh`), get mined into candidate rules that start in shadow mode (they
run and get tracked, but don't surface findings) until they've proven
themselves against real feedback.

```
autoreview rules mine
autoreview rules review
```

## Commands

| Command | What it does |
|---|---|
| `diff` | Review a diff. The main entry point. |
| `doctor` | Check which tools are available and what that means for coverage. |
| `apply` | Apply a finding's suggested patch. |
| `feedback` | Record true/false-positive verdicts on findings. |
| `rules` | Mine, review, bench, and manage the learned rule factory and external rule packs. |
| `skills` | Manage the per-aspect instructions LLM specialists follow, including learned edits to them. |
| `history` | Manage local run and event history, including team sync. |

Run `autoreview <command> --help` for the full options on any of them.

## Development

```
cargo build --workspace --tests
cargo test --workspace
cargo clippy --workspace --tests
```

Tests that depend on `ast-grep` or `golangci-lint` skip themselves cleanly
when those tools aren't on `PATH`, printing what they skipped rather than
failing.
