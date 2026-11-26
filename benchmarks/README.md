# Historical replay cases

These cases reference public repositories and pinned commits; they do not
copy the source files into Autoreview. Check each project's license before
adding cases. Labels cover an entire file, not only the changed lines.

For cross-file findings, each `before` or `after` label may specify an
optional `entry` path. Autoreview analyzes that file and its supported
dependencies, while `path` and `lines` identify the file containing the
expected findings. Both paths refer to regular repository files in the named
snapshot. If `entry` is omitted, it defaults to `path`. Findings in other
files are not scored by that label; use separate cases when those files also
need labels. An incomplete analysis still excludes the case from scoring.

The Apache-2.0 Iceberg case records a Testify assertion comparing a range
value with the same slice element. The `go-testify-range-element-self-comparison`
framework check now finds the labeled line before the fix and no findings in the
fixed file. This single historical fix pair is too small to estimate general
precision or recall. The check is intentionally limited to a locally declared,
known slice and conservative syntactic mutation and shadowing checks; it does
not prove absence of indirect mutation or model arbitrary aliases.

To reproduce with Git and Autoreview installed, run from a temporary directory:

```sh
git init iceberg-go
cd iceberg-go
git fetch --depth 2 https://github.com/apache/iceberg-go.git b66585bfee314c8676ad1bff30b5162de4314338
autoreview rules replay --suite /absolute/path/to/autoreview/benchmarks/iceberg-go.yaml
```

The fix commit and its immediate parent must both be available. Use the
report's historical and synthetic breakdowns separately, inspect failures,
and do not interpret an unrepresentative sample as population-wide accuracy.

The replay report's `coverage` entries group cases by rule and language.
`attempted` counts all cases, `completed` counts cases whose two revisions were
analyzed, `incomplete` counts cases rejected because dataflow reported a
coverage limitation, and `failed` counts other errors such as missing commits.
A mismatch between findings and labels is a completed case and appears in the
accuracy scores. Incomplete and failed cases are excluded from precision and
recall, and make replay exit unsuccessfully. These are case counts, not file
or finding counts.

For an incomplete dataflow case, each entry in `failures` includes an
`analysisCoverage` array with the analyzer, reason, scope, and available
path, line, and rule. The `error` text also includes line numbers when known.
Replay stops that case at the first failing revision, so these details do not
imply that the other revision was analyzed.

`azure-iot-matches.yaml` records an MIT-licensed Rust test fix in the Azure IoT
Operations SDK. Six `test_case` tests discarded `matches!` predicates before
the fix; replay detects all six and none in the fixed file. This is one fix
pair, not a representative accuracy estimate. Reproduce it with:

```sh
git init azure-iot
cd azure-iot
git fetch --depth 2 https://github.com/Azure/iot-operations-sdks.git 9c5c20f714caedc7fda71df9032469b5da1e04ce
autoreview rules replay --suite /absolute/path/to/autoreview/benchmarks/azure-iot-matches.yaml
```

`jplag-trim.yaml` is an independent GPL-3.0 Java fix pair from JPlag. A
standalone `String.trim()` result was discarded before a filter entry was
lowercased; the fix uses the trimmed string. Replay of the pinned parent and
fix detects line 37 before the fix and no findings in the corrected file. This
is one positive and one fixed-file example, not a general accuracy estimate.
The manifest references the repository and does not redistribute its source.
Reproduce it with:

```sh
git init jplag
cd jplag
git fetch --depth 2 https://github.com/jplag/JPlag.git 224f9d73d1d1250447fc700a8d553703afb91091
autoreview rules replay --suite /absolute/path/to/autoreview/benchmarks/jplag-trim.yaml
```

`go-tools.yaml` is a historical negative case for a test backup rename whose
error is explicitly ignored. The label treats this best-effort test operation
as acceptable; teams may make a different policy choice. Before Go helper
discovery filtered unrelated package files, replay reached the dependency
limit. With the filter, both revisions replay without findings or reported
coverage limits. This negative-only example does not measure recall or
establish precision. Reproduce it with:

```sh
git init go-tools
cd go-tools
git fetch --depth 2 https://github.com/golang/tools.git c6f2f8edba4a9d5be665bb151d141365e03eeb6e
autoreview rules replay --suite /absolute/path/to/autoreview/benchmarks/go-tools.yaml
```

`syzkaller-swallowed-error.yaml` records an Apache-2.0 Go fix where a failed
service-configuration call was checked but the branch returned `nil`. Replay
finds line 188 before the fix and no findings in the fixed file. This is one
historical fix pair, not an estimate of general rule accuracy. The rule is a
syntactic heuristic: it does not prove the checked value's type or whether
suppressing the error is intentional. It only matches an immediately returning
branch inside an explicitly `error`-returning function or method, and excludes
branches with additional handling. Reproduce with:

```sh
git init syzkaller
cd syzkaller
git fetch --depth 2 https://github.com/google/syzkaller.git 9e5c2a555caa9213219160895411f922d6c0358b
autoreview rules replay --suite /absolute/path/to/autoreview/benchmarks/syzkaller-swallowed-error.yaml
```

`cilium-swallowed-error.yaml` records a second, independent Apache-2.0 fix for
the same rule. The checked IPv6 chain rename error was returned as `nil`;
replay finds line 172 in the parent and no findings in the fixed file. It also
labels normal context cancellation in `pkg/datapath/iptables/ipset/node_sync.go`
as an intentional `nil` return. The rule currently flags line 101 in both
revisions, so replay reports one true positive and two false positives and
exits nonzero. The two false positives are the same unchanged code, not
independent examples. The cancellation label follows the local limiter's
one-token, burst-one configuration and the surrounding shutdown handling; it is
a policy judgment, not proof of intent. These cases are too few to estimate
general accuracy. Reproduce with:

```sh
git init cilium
cd cilium
git fetch --depth 2 https://github.com/cilium/cilium.git fb7b8f905fc3e00fa17aa510b0e2d9262c469053
autoreview rules replay --suite /absolute/path/to/autoreview/benchmarks/cilium-swallowed-error.yaml
```

Coverage reporting is not exhaustive: unsupported expressions and unresolved
imports can still escape the current diagnostics. A clean replay is evidence
for this case, not a guarantee of complete analysis in general.

`uwas-rename.yaml` records an independent Go error-handling fix in UWAS,
dual licensed under AGPL-3.0-or-later or a commercial license. Session
persistence discarded an `os.Rename` error; the fix logs it. An earlier
rename in the same file already returns its error. The file-level replay
detects the discarded result before the fix and no findings afterward: one
case attempted and completed, with one true positive, no false positives or
false negatives, and no reported incomplete analysis. This single fix pair
does not establish general precision or recall, and logging the error does not
prove that persistence can recover from it. The manifest references the
upstream commits without redistributing the source. Reproduce it with:

```sh
git init uwas
cd uwas
git fetch --depth 2 https://github.com/uwaserver/uwas.git 72ff1f5897e89e6d38ecb9d7a51712ace23afe17
autoreview rules replay --suite /absolute/path/to/autoreview/benchmarks/uwas-rename.yaml
```

`niverwizard-msw-delay.yaml` records one MIT-licensed TypeScript fix across
three mock-handler files. The built-in MSW delay rule finds all four labelled
discarded promises before the fix and none in the corrected files; all three
cases complete without reported coverage limitations. They share one fix
commit and are not independent fixes or a general accuracy estimate. The
rule uses MSW's documented delay contract and does not need project packages
or a TypeScript compiler. Reproduce with:

```sh
git init niverwizard
cd niverwizard
git fetch --depth 2 https://github.com/kiLeo13/NiverWizard.git fec608d7dfc4a763079bf9a02ab31d4f38fb17b4
autoreview rules replay --suite /absolute/path/to/autoreview/benchmarks/niverwizard-msw-delay.yaml
```

The experimental general typed-promise rule was also tried on these files
with installed dependencies and a separately configured compatible compiler.
Replay reported all three cases incomplete because other call types could
not be classified; that experiment is not scored. The project pins
TypeScript 7, whose package does not expose the compiler API expected by the
current probe. This API-specific replay does not establish general support for
that compiler or project-wide promise analysis. A compatible compiler can use
`AUTOREVIEW_TYPESCRIPT_CONFIG=frontend/tsconfig.app.json` to select each
snapshot's nested configuration without a configuration adapter.

`checkout-promise.yaml` exercises the experimental general promise source on
the MIT-licensed actions/checkout fix documented in `docs/benchmark-candidates.md`.
The normal replay currently exits nonzero and marks both cases incomplete due
to unresolved call types; they are not included in accuracy scores.

On main at `8af2697` with TypeScript 5.6.3 and the setup below, the before
revision reported three unresolved call locations for the deletion case and 21
for the token-replacement case, including imported dependencies. Examples are
`src/github-api-helper.ts:39` and `src/fs-helper.ts:10`. These counts describe
this run's reported issues, not precision or recall, and the first failure in
each case prevented replay from evaluating its after revision.

To reproduce from a fresh clone, set `AUTOREVIEW_ROOT` to the absolute path of
this Autoreview checkout and run:

```sh
git clone https://github.com/actions/checkout.git checkout
cd checkout
git checkout --detach 5a4ac9002d0be2fb38bd78e4b4dbde5606d7042f
npm ci --ignore-scripts --no-audit --no-fund
npm install --prefix "$PWD/../ts-5.6.3" --no-save --ignore-scripts --no-audit --no-fund typescript@5.6.3
mkdir -p .autoreview .autoreview-replay-tmp
printf 'packs:\n  - id: checkout-promise-benchmark\n    source:\n      kind: local\n      path: "%s/benchmarks/packs/checkout-promise"\n' "$AUTOREVIEW_ROOT" > .autoreview/rulepacks.yaml
TMPDIR="$PWD/.autoreview-replay-tmp" \
AUTOREVIEW_BENCHMARK_TYPESCRIPT_MODULE="$PWD/../ts-5.6.3/node_modules/typescript" \
AUTOREVIEW_TYPESCRIPT_MODULE="$AUTOREVIEW_ROOT/benchmarks/typescript-legacy-catch.cjs" \
  autoreview rules replay --suite "$AUTOREVIEW_ROOT/benchmarks/checkout-promise.yaml"
```

The benchmark-only adapter sets `useUnknownInCatchVariables` to `false`, the
older project's catch-variable behavior, while leaving other project options
and declaration diagnostics enabled. It executes the explicitly configured
compiler package, so both paths must point to trusted code. Replay snapshots
are placed under the clone so their imports resolve the installed dependencies.
Do not reuse that dependency setup for different revisions without checking the
manifests and configuration.

`sftp-promise.yaml` labels two discarded promises in one TypeScript file and
the fixed file. On the typed-promise branch, a replay found both before-fix
lines and no findings after the fix, with no reported replay failures. This is
one historical fix pair and cannot establish general precision or recall.

The checker needs trusted, installed TypeScript dependencies. The two pinned
revisions have identical `package.json`, `package-lock.json`, and
`tsconfig.json`. One way to reproduce the run is to check out the fixed
revision in an isolated SFTPresso clone, install its lockfile dependencies
without running package scripts, and place replay snapshots beneath the clone
so TypeScript can resolve those dependencies from their parent directory:

```sh
git checkout --detach 460fa47433bf6ed423cc84cb0939ce6b90246e6a
npm ci --ignore-scripts --no-audit --no-fund
mkdir -p .autoreview-replay-tmp
TMPDIR="$PWD/.autoreview-replay-tmp" \
AUTOREVIEW_TYPESCRIPT_MODULE="$PWD/node_modules/typescript" \
  autoreview rules replay --suite /absolute/path/to/autoreview/benchmarks/sftp-promise.yaml
```

The configured TypeScript package executes as part of the check; use only a
trusted installation. Replay rejects reported incomplete analysis rather than
counting it as a clean result. Do not reuse this dependency setup for other
revisions unless their dependency configuration also matches.


`yq-error.yaml` labels three discarded errors before one historical fix and
none in the fixed file. With the experimental Go checker configured and the
pinned dependencies cached, replay detects all three before the fix and none
after it. This is one historical fix pair, not a general accuracy estimate.
The two revisions have identical dependency manifests. For an isolated checkout
containing both commits, download the pinned modules into an isolated cache,
then disable downloads for replay:

```sh
export GOMODCACHE=/absolute/path/to/isolated-cache
GOTOOLCHAIN=local go mod download all
cd /absolute/path/to/autoreview/tools/go-error-evidence
go build -o /absolute/path/to/trusted-go-error-probe .
cd /absolute/path/to/yq-checkout
AUTOREVIEW_GO_ERROR_PROBE=/absolute/path/to/trusted-go-error-probe \
AUTOREVIEW_GO_BINARY=/absolute/path/to/trusted/go \
GOPROXY=off GOSUMDB=off \
  autoreview rules replay --suite /absolute/path/to/autoreview/benchmarks/yq-error.yaml
```

The Go checker can execute build tools and requires a trusted project and
compiler. It does not yet support test variants, cgo target packages, explicit
build tags, full snapshot verification, or process-tree containment.

`yq-multi-error.yaml` labels five calls that discard both the byte count and
error returned by `Write`, plus the corrected file. With the version 2 Go
probe, the replay detects all five before the fix and none after it. Its two
revisions have identical Go dependency manifests. Use the same isolated-cache
procedure as `yq-error.yaml`, checking out commit
`25293a6894a8bcbec0fca3acc8eda2171fd72ecc` and its parent, and selecting this
suite. This is one additional fix pair, not a population-wide accuracy estimate.
