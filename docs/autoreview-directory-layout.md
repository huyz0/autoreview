# The `.autoreview/` directory

Everything under `.autoreview/` is per-repo configuration and state, meant to
be committed alongside the code it governs. It's entirely opt-in: a repo with
no `.autoreview/` directory at all runs `autoreview diff` with sensible
defaults — every file below follows the same "missing file/directory means
'use the default' or 'feature off', not an error" convention.

This is distinct from `~/.cache/autoreview/<fingerprint>/`, autoreview's own
machine-local, gitignored working directory (review history, event logs, git
sync clones, run reports) — see [Where history and reports actually
live](#where-history-and-reports-actually-live) at the bottom.

```
.autoreview/
├── config.yaml                          # main config
├── architecture.yaml                    # layer/import rules
├── rulepacks.yaml                       # registered external rule packs
├── spec.md                              # optional acceptance-criteria spec
├── context/                             # free-form docs fed to specialists
├── rules/
│   ├── candidates/<clusterId>/seed.json #   mined rule candidates, pending review
│   ├── shadow/<id>.yaml                 #   approved, running but not surfaced
│   ├── promoted/<id>.yaml               #   graduated, findings surface normally
│   └── rejected/<id>.yaml               #   declined candidates, kept for history
└── skills/
    └── <aspect>/
        ├── instructions.md              #   repo-local override of a built-in skill
        └── proposals/<clusterId>.md     #   mined skill-edit proposals, pending review
```

## `config.yaml`

The main config file, read by every `autoreview diff` run. All fields are
optional — an absent `config.yaml`, or an absent section within one, falls
back to `AutoreviewConfig::default()`
([`crates/schema/src/config.rs`](../crates/schema/src/config.rs)). Top-level
sections:

- `triage` — the scoring inputs that pick a review tier (`quick`/`standard`/`deep`).
- `budgets` — per-tier model choice and spend/agent-count caps.
- `context` — which context providers feed a specialist's prompt (defaults to
  auto-discovering `CLAUDE.md`, `CONTRIBUTING.md`, `docs/adr/**`, and this
  repo's own `.autoreview/context/**`, plus recent git history).
- `storage` — where history lives, sync mode (`none`/`git`/`remote`), FP/TP
  override thresholds.
- `verify` — Stage 3.5's cheap-model confirmation pass: which analyzer
  categories get a second look by default (`noisyCategories`).
- `agents` — which backend drives specialists
  (`claude-code`/`pi`/`local-llm`/`openai-compatible`) and its settings.
  The hosted `openai-compatible` backend reads its API key from the
  credential store, never from this file — see `auth` below.
- `symindex` — cross-file symbol-index tuning (e.g. `tier4_go`).
- `auth` — non-secret auth settings only, e.g. `github.clientId` for the
  OAuth device flow. Tokens themselves never live here; they go to the OS
  keyring (or a locked-down local file) via `autoreview auth login`.
- `mineFromComments` — opt-in GitHub PR-review-comment mining (see `rules
  mine --from-comments`).
- `mineFromBugfixCommits` — how many commits `rules mine
  --from-bugfix-commits` scans. No `enabled` flag: it only reads local git
  history, so there's no network or auth to opt into.
- `mineFromBitbucketComments` — opt-in Bitbucket Cloud PR-comment mining
  (`rules mine --from-bitbucket-comments`). The `workspace` slug is
  repo-level shared config and belongs here; the Bitbucket credential does
  not.
- `mineFromLlmPatterns` — opt-in LLM-assisted call-pair convention mining
  (`rules mine --from-llm-patterns`). Off by default for privacy: unlike
  every other source, it sends whole sampled file contents to the
  configured agent backend.

## `architecture.yaml`

Declares named layers (glob patterns over file paths) and `forbid` rules
between them — e.g. "the `repository` layer may never import from
`handler`". Checked per changed file's own imports (Go, Java/Kotlin,
TypeScript/JavaScript) by
[`analyzers::architecture`](../crates/core/src/analyzers/architecture.rs); a
direct-import check only, not a transitive one (that's what `archgraph`, Tier
2's real dependency graph, is for). No file means no layers are declared, so
nothing is ever flagged.

```yaml
architecture:
  layers:
    - name: handler
      match: ["**/handler/**"]
    - name: repository
      match: ["**/repository/**"]
  rules:
    - forbid:
        from: repository
        to: [handler]
```

## `rulepacks.yaml`

Registers external rule packs — third-party rule bundles this repo points
autoreview at without forking the binary. Managed via `autoreview rules
packs add <source>` / `validate <path>` / `refresh` / (no subcommand) to
list; see
[`crates/core/src/rule_packs/mod.rs`](../crates/core/src/rule_packs/mod.rs)
for the full resolution model (local paths vs. git URLs, `trust: full` vs.
`shadow`).

```yaml
packs:
  - id: acme-security
    source:
      kind: local
      path: ../shared-rules/acme-security
  - id: acme-perf
    source:
      kind: git
      url: https://github.com/acme/perf-rules
      ref: v1.2.0
    trust: shadow
```

## `spec.md`

An optional, free-form spec for the change under review: `# Title`, an
`## Intent` section, and an `## Acceptance Criteria` bullet list. When
present, each criterion is checked against the diff by an LLM judge —
a genuinely different question from "is this code clean" (what every other
stage asks), so it's additive to the finding-based review, not a
replacement. See
[`spec_verify/mod.rs`](../crates/core/src/spec_verify/mod.rs).

## `context/`

Free-form markdown/text files (ADRs, style notes, design docs) automatically
picked up by the default `context` provider and fed into specialist prompts,
subject to a per-item and total char budget. No naming convention — anything
under here matches the default `.autoreview/context/**` glob.

## `rules/`

The lifecycle a mined or hand-authored rule moves through, one directory per
stage — `autoreview rules mine` / `review --approve` / the automatic
promote/demote gate in `diff.rs` / `rules rollback` all move a rule's file
between these directories (see
[`crates/cli/src/commands/rules.rs`](../crates/cli/src/commands/rules.rs)):

1. **`candidates/<clusterId>/seed.json`** — a mined candidate, not yet a real
   rule file, awaiting `rules review --approve`/`--reject`.
2. **`shadow/<id>.yaml`** — an approved rule, running on every `diff` and
   recorded to history, but its findings are suppressed from the surfaced
   report until it earns enough true-positive agreement.
3. **`promoted/<id>.yaml`** — graduated: findings surface normally, like a
   builtin rule.
4. **`rejected/<id>.yaml`** — a candidate or shadow rule that was declined or
   rolled all the way back; kept (not deleted) so the decision has a record.

A promoted rule can demote back to shadow (or a shadow rule reject) either
automatically (the firing-history gate in `diff.rs`) or manually (`autoreview
rules rollback <id>`).

## `skills/`

Per-aspect (`correctness`, `design`, `security`, `performance`, ...)
instructions the LLM specialists use, and the mining pipeline that proposes
edits to them:

- **`<aspect>/instructions.md`** — a repo-local override of that aspect's
  built-in skill. If absent, the built-in instructions apply unmodified. See
  [`crates/core/src/skills/mod.rs`](../crates/core/src/skills/mod.rs).
- **`<aspect>/proposals/<clusterId>.md`** — a mined skill-edit proposal
  (recurring feedback pattern suggesting the skill's own instructions should
  change), awaiting `autoreview skills review --approve`/`--reject`. An
  approved proposal appends to (creating, if absent) that aspect's own
  `instructions.md`, versioned in history for `skills rollback`.

## Where history and reports actually live

Review history (the SQLite store backing shadow-rule tracking, FP/TP
feedback, skill versions), per-run JSON/Markdown/SARIF reports, and git-sync
clones are **not** under `.autoreview/` — they live in a machine-local,
gitignored cache directory keyed by a fingerprint of the repo (path + remote
URL), under the OS cache dir (`~/.cache/autoreview/<fingerprint>/` on Linux):

```
~/.cache/autoreview/<fingerprint>/
├── index.db          # SQLite history store
├── events/            # append-only per-host event log (storage.sync)
├── runs/<runId>/      # report.json / report.md / report.sarif per run
└── rulepacks/<hash>/  # git-source rule pack clones (shared across repos)
```

This split is deliberate: `.autoreview/` is the part meant to be reviewed and
versioned with the code (config, rules, skills — decisions a team makes
together), while the cache directory is disposable, machine-local working
state that nothing depends on being backed up. `storage.sync.mode:
git`/`remote` (in `config.yaml`) is what lets a *team* share the event log
across machines without putting any of it in the repo itself. See
[`crates/cli/src/commands/history.rs`](../crates/cli/src/commands/history.rs)
(`history_dir_for`) for the exact fingerprinting logic.
