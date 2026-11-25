# Historical replay cases

These cases reference public repositories and pinned commits; they do not
copy the source files into Autoreview. Check each project's license before
adding cases. Labels cover an entire file, not only the changed lines.

The initial corpus contains one Apache-2.0 Go case. It is too small to
estimate general accuracy. On main commit `6f2f63b`, with ast-grep installed,
the existing Testify rule misses the labelled line: one false negative,
zero true positives, and zero false positives. Precision is undefined for
this sample, and recall is zero for this one label. The replay exits nonzero
because the actual result differs from the label; that exit is expected until
the rule can follow this alias.

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

Coverage reporting is not exhaustive: unsupported expressions and unresolved
imports can still escape the current diagnostics. A clean replay is evidence
for this case, not a guarantee of complete analysis in general.

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
