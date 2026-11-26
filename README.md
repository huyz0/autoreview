# autoreview

A portable, deterministic-first code review CLI. It reviews a git diff the
way a careful senior engineer would: run the cheap, reliable checks first
(ast-grep patterns, dataflow/taint analysis, complexity metrics, import-cycle
detection), and only spend LLM budget on the parts those checks can't cover.

Deterministic analyzers provide reproducible evidence. Heuristic checks can
still produce false positives. LLM specialists get called in proportion to how risky the diff actually looks,
scored by a triage step that weighs file count, churn, test coverage, and how
sensitive the touched paths are. A small change to a well-tested utility
function gets a quick pass; a large change to authentication code gets the
deep tier.

## What it checks

- **ast-grep pattern rules** for Go, Java, Kotlin, TypeScript, JavaScript,
  and Rust. Rust rules cover resource lifetimes, panic and unsafe preconditions,
  blocking behavior, credentials, and TLS.
- **Scoped Rust review** resolves explicit imports and local bindings to catch
  discarded assertions in supported parameterized and asynchronous tests, Mockall expectation problems, and
  SQLx, Diesel, and rusqlite query issues without a build.
- **Control-flow analysis** runs taint and call-sequence rules for Go, Java,
  Kotlin, JavaScript, TypeScript, TSX, and Rust. It can trace an HTTP parameter to a
  command execution call or check whether code closes a resource before reuse.
  Go also checks whether typed nil pointers are returned through interfaces.
- **Typed promise checks** can flag unused results from imported TypeScript
  and TSX functions created by a typed factory. Set
  `AUTOREVIEW_TYPESCRIPT_MODULE` to the absolute path of a trusted TypeScript
  installation to enable type verification; Node must also be available.
  The repository-root `tsconfig.json` is used unless
  `AUTOREVIEW_TYPESCRIPT_CONFIG` names an absolute config path. Without a
  configured checker, coverage reports identify the skipped rule. This check
  only covers supported, unambiguous factory and import shapes.
- **Experimental Go error checks** can flag unused `error` results from type-checked calls, including
  supported multi-result assignments and calls passed to uniquely resolved helpers.
  Unresolved receiving helpers are reported as incomplete. Build the probe in `tools/go-error-evidence` and set
  `AUTOREVIEW_GO_ERROR_PROBE` and `AUTOREVIEW_GO_BINARY` to absolute paths of
  trusted executables. Package loading runs Go build tools against the project;
  use only trusted projects. Missing configuration or incomplete analysis is
  reported as coverage limitations. Test packages, cgo, explicit build tags,
  full snapshot verification, and process-tree containment remain unsupported.
- **Framework checks** flag Go context cancellation missed on a branch or
  through a same-package helper, Java Spring transaction self-invocation,
  and Hibernate reads that initialize a mapped lazy collection for each
  result of a typed query. The Spring check assumes proxy mode. The Hibernate
  check reports a query pattern that should be confirmed in generated SQL.
  Java constructor dispatch and Spring self-invocation trace superclass and
  interface methods declared in the repository. Findings point to the resolved
  declaration or transaction annotation, including in another file.
- **Telemetry checks** flag likely high-cardinality Prometheus and Micrometer
  labels, Go OpenTelemetry spans that can exit without ending, and Java
  OpenTelemetry scopes discarded without a cleanup owner.
- **Test checks** flag equality assertions that compare the same variable
  in supported Testify, JUnit, Kotlin Test, Jest-style, and Rust APIs.
- **Complexity metrics** with YAML-configurable thresholds. Java and Rust use
  syntax trees to measure function length, parameters, nesting, branches, and
  explicit returns. Rust also checks required trait items and oversized
  modules. The module rule looks for more than 20 direct declarations and
  at least 300 lines of code in a source file or inline module. Nested
  modules and test-only items are measured separately or excluded.
  Rust feature envy reports an inherent method that uses at least three
  distinct fields of one typed parameter and accesses at least two fewer
  members on its own receiver.
- **Cross-file structure**: import-cycle detection (Go, Java, Kotlin,
  JavaScript, TypeScript, and TSX),
  ArchUnit-style layer rules from `.autoreview/architecture.yaml`, symbol-index-backed
  smells like feature envy and data clumps. Cargo metadata resolves direct
  Rust workspace package boundaries and the build scripts that Cargo runs.
- **golangci-lint and clippy** report into the same finding format. Autoreview
  classifies selected Clippy lints as correctness, performance, or design for
  `--aspects` filtering. Other Clippy lints appear under style.
- **Rust dependency advisories** via optional offline `cargo audit` for
  changed lockfiles.
- **LLM specialists** for the things only a model can judge: does this
  actually do what the PR claims, is this abstraction the right shape, is
  this test meaningful or just padding coverage.

Every finding a rule produces gets deduplicated against your own review
history, so the same rule doesn't nag you about the same code twice, and a
rule with too many false positives on your codebase gets flagged for
tuning instead of silently eroding trust in the tool.

## Install

Released binaries cover macOS (Apple Silicon and Intel) and x86_64 Linux.
The Linux build is statically linked against musl, so it has no glibc
version requirement and runs on Alpine as happily as on Ubuntu.

**Script** — downloads the right build for your platform and verifies its
published SHA-256 before installing:

```bash
curl -fsSL https://raw.githubusercontent.com/huyz0/autoreview/main/packaging/install.sh | sh
```

Installs to `~/.local/bin` by default; set `AUTOREVIEW_BIN_DIR` to change
that, or `AUTOREVIEW_VERSION` to pin a tag instead of taking the latest.
If piping a script into a shell isn't to your taste, read it first — it's
[`packaging/install.sh`](packaging/install.sh), about 100 lines of POSIX
sh — or use the manual route below.

**Homebrew**:

```bash
brew tap huyz0/tap
brew install huyz0/tap/autoreview
```

Each release also attaches the formula directly, if you'd rather not add
a tap:

```bash
brew install --formula https://github.com/huyz0/autoreview/releases/latest/download/autoreview.rb
```

**Manual** — grab a tarball from the
[releases page](https://github.com/huyz0/autoreview/releases), check it
against the published `checksums.txt`, and put the binary on your `PATH`.

**From source** — needs a Rust toolchain:

```bash
git clone https://github.com/huyz0/autoreview
cd autoreview
cargo build --release
```

The binary lands at `target/release/autoreview`.

Whichever route you take, run `autoreview doctor` afterwards: several
analyzers shell out to tools (`ast-grep`, `golangci-lint`) that aren't
bundled. The review reports missing or failed tools so you can see when
coverage was reduced.

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
of the run). Source files are read from the selected head commit in a temporary
checkout. Repository-local settings and rule registrations come from your checkout.
For a review without model calls, pass `--static-only`. Add `--changed-lines`
to keep deterministic findings whose primary or related location overlaps
added or modified lines. By default, deterministic checks report across touched
files, including findings whose locations are outside the changed hunks.

The Java nullability regression check can also report a dereference in an
unchanged caller file when a repository-local static helper gains a direct
null return. The report links the caller, its helper call, and the changed
helper. The initial check resolves class-qualified calls with a unique
signature, follows local aliases and null guards, and recognizes selected
instance methods on strings and repository-local types.

To check Rust dependencies in changed `Cargo.lock` files, pass
`--audit-rust-deps`. Install [cargo-audit](https://github.com/rustsec/rustsec/tree/main/cargo-audit)
and populate its local RustSec database before running the review. Autoreview
uses that database offline and reports scanner failures as analyzer warnings.
The audit covers Cargo's default registry. With `--changed-lines`, the
affected package's name or version range must overlap the diff.

Record feedback on a finding to teach the rule factory what's
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
dataflow tracking, `kind: threshold` for metrics, `kind: call-sequence` for
must-follow call ordering like lock/unlock), not Rust code, so adding one
doesn't mean recompiling. Point autoreview at a shared or third-party rule
pack without forking anything:

```
autoreview rules packs add https://github.com/your-org/rule-pack
autoreview rules packs validate ../rule-pack-in-progress
```

autoreview can also learn rules from your own history. `rules mine` reads
recurring patterns in what specialists have flagged; several opt-in flags
mine other evidence instead:

| Flag | Evidence |
|---|---|
| `--from-comments` | recurring human PR review comments (GitHub, via `gh`) |
| `--from-bitbucket-comments` | the same, from Bitbucket Cloud |
| `--from-bugfix-commits` | bug-fix-shaped commits in local git history |
| `--from-suppressions` | `// nolint` / `@SuppressWarnings` / `// eslint-disable` comments already in your source |
| `--from-code` | call-pair conventions the codebase already follows |
| `--from-llm-patterns` | conventions an agent proposes from reading sampled files, each mechanically re-verified against the whole repo before it counts |
| `--from-linter-config` | a report comparing your `.golangci.yml`/`.eslintrc`/checkstyle/detekt config against autoreview's own catalog |

Whatever the source, candidates go through the same gate: drafted, benched
against fixtures, approved by a human, then run in shadow mode (tracked but
not surfaced) until they've proven themselves against real feedback.

```
autoreview rules mine
autoreview rules review
```

### Draft a rule from one finding

```sh
autoreview rules draft <finding-id> --fixed <fix-commit>
autoreview rules draft <finding-id> --proposal proposal.yaml --fixed <fix-commit>
```

The first command sends bounded excerpts from the recorded commits to your
configured model. Use `--proposal` to verify a saved rule without calling a
model. Autoreview checks the proposed rule against the reported lines with
its real analyzer and, when you supply `--fixed`, requires the same file to
be clean at that commit. Successful drafts are saved as unregistered packs
under `.autoreview/rules/drafts/`. Review their scope before registering one.
A threshold rule changes the corresponding repository-wide metric.
See the [rule DSL reference](.claude/skills/authoring-rules/reference/rule-dsl.md)
for supported rule kinds.

### Replay rules and opt into a merge gate

Use `autoreview rules replay` to compare a built-in rule or a registered custom
call-sequence rule's findings with
hand-labelled lines in complete files at known-bad and fixed Git commits.
The JSON report shows precision, recall, and the lines each case missed or
reported unexpectedly. Set `--suite` to use a different YAML file.

`autoreview rules gate --base origin/main --head HEAD` is an optional CI
check. It uses the base commit's rule allowlist and replay evidence, then
fails on findings from qualified rules that overlap changed lines. Design
and style findings remain advisory. The repository needs enough Git history
for the comparison and replay commits. [Suite format and gate criteria](docs/autoreview-directory-layout.md#replayyaml)

## Language coverage

| Check | Go | Java / Kotlin | JavaScript / TypeScript / TSX | Rust |
|---|---|---|---|---|
| Syntax patterns and complexity | Yes | Yes | Yes | Yes |
| Taint and call-sequence DSL | Yes | Yes | Yes | Supported local and cross-file subset |
| Feature envy | Symbol index | Symbol index | Symbol index | Typed parameters in inherent methods |
| Layer policies | Imports | Imports | Imports | Direct Cargo workspace dependencies |
| Import cycles | Go packages | JVM packages | Relative module imports | Unavailable |

JavaScript and TypeScript analysis and triage recognize JSX, TSX, and
module extensions such as `.mjs` and `.mts`. Rules with
`language: TypeScript` also run on TSX. Use `language: Tsx` to target only
TSX. [DSL reference](.claude/skills/authoring-rules/reference/rule-dsl.md)

Rust call-sequence helper tracking follows supported explicit imports and
unambiguous `crate::`, `self::`, and `super::` module aliases, and supported
named or wildcard `pub use` re-exports. Conditional modules, custom paths,
ambiguous crate roots, and other import forms can limit coverage; a used
unsupported module alias is reported as incomplete where recognized.

Web cycle checks follow static relative imports and re-exports, plus direct
top-level `require` calls in `.cjs` and `.cts` files. They skip type-only
imports, dynamic imports, and paths requiring package or alias configuration.

See [antipattern coverage](docs/antipattern-coverage.md) for the current checks
and the evidence needed to expand them.

## Driving autoreview from a coding agent

This repo ships two skills under `.claude/skills/`, picked up automatically
by any agent harness that reads them:

- [`running-reviews`](.claude/skills/running-reviews/SKILL.md) — run a
  review and act on it: interpret the report, explain why a finding fired,
  apply a suggested patch, and record verdicts without corrupting the
  rule-learning loop.
- [`authoring-rules`](.claude/skills/authoring-rules/SKILL.md) — turn a
  recurring pattern into a new deterministic rule, with the DSL reference
  and the validate/register workflow.

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
| `auth` | Log in to GitHub / Bitbucket / an OpenAI-compatible provider; tokens go to the OS keyring. |

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

## License

Apache License 2.0 — see [LICENSE](LICENSE).
