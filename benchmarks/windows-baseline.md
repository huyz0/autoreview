# Windows historical replay baseline

## Current run: main 340d40c

On Windows, the debug CLI and Go evidence probe built from main commit
`340d40c60f1288088e312c77cf00ef9129158001` replayed the pinned manifests
below. Git was 2.56.0.windows.1, Rust 1.99.1, ast-grep 0.51.3, Go 1.27.4,
and Node 26.12.1. SFTPresso used TypeScript 5.9.3; checkout used TypeScript
5.6.3 with the documented legacy-catch adapter and benchmark rule pack.
The yq runs used their separate cached dependency sets with downloads disabled.
Git long-path support was enabled for this run.

| Manifest | Completed / attempted | Incomplete | Failed | TP | FP | FN |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `azure-iot-matches.yaml` | 1 / 1 | 0 | 0 | 6 | 0 | 0 |
| `iceberg-go.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `jplag-trim.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `go-tools.yaml` | 1 / 1 | 0 | 0 | 0 | 0 | 0 |
| `uwas-rename.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `niverwizard-msw-delay.yaml` | 3 / 3 | 0 | 0 | 4 | 0 | 0 |
| `syzkaller-swallowed-error.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `cilium-swallowed-error.yaml` | 2 / 2 | 0 | 0 | 1 | 2 | 0 |
| `silphscope-digest.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `yq-error.yaml` | 1 / 1 | 0 | 0 | 3 | 0 | 0 |
| `yq-multi-error.yaml` | 1 / 1 | 0 | 0 | 5 | 0 | 0 |
| `sftp-promise.yaml` | 1 / 1 | 0 | 0 | 2 | 0 | 0 |
| `checkout-promise.yaml` | 0 / 2 | 2 | 0 | — | — | — |
| `redicloud-coroutines.yaml` | 0 / 1 | 1 | 0 | — | — | — |
| `sentry-async-executor.yaml` | 1 / 1 | 0 | 0 | 2 | 0 | 0 |

The yq multi-result case completes on this revision. Cilium still reports the
same cancellation-handling line in both revisions, accounting for the two
false positives; these are not independent defects. Checkout reports two and
21 `promise_type_unresolved` locations in its respective before revisions.
RediCloud reports `callback_execution_unresolved` at line 120 of its before
revision. Replay stops each incomplete case before evaluating its after
revision, so none of these three cases contributes to accuracy scores.

These results suggest prioritizing promise type resolution and callback
execution evidence, followed by call-specific error-path analysis for the
Cilium mismatch. They also expose a measurement gap: this small, selected
corpus does not cover most rule families, and multiple findings in the same
fix pair or repeated revisions are not independent samples. Add independent
fixes and clean examples before drawing conclusions about general precision
or recall. See [the benchmark README](README.md) for setup and reproduction.

## Earlier run: main 340d331

On Windows, the debug CLI built from main commit
`340d3315518e605aa04f86f5ca73a5683ce4a4a0` replayed the pinned manifests
below using Git 2.56.0.windows.1, Rust 1.99.1, ast-grep 0.51.3, Go 1.27.4,
and Node 26.12.1. The Go probe was rebuilt from this commit. The SFTPresso
case used TypeScript 5.9.3; the checkout cases used TypeScript 5.6.3 with
the documented legacy-catch adapter and the benchmark rule pack.

| Manifest | Completed / attempted | Incomplete | Failed | TP | FP | FN |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `azure-iot-matches.yaml` | 1 / 1 | 0 | 0 | 6 | 0 | 0 |
| `iceberg-go.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `jplag-trim.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `go-tools.yaml` | 1 / 1 | 0 | 0 | 0 | 0 | 0 |
| `uwas-rename.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `niverwizard-msw-delay.yaml` | 3 / 3 | 0 | 0 | 4 | 0 | 0 |
| `syzkaller-swallowed-error.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `cilium-swallowed-error.yaml` | 2 / 2 | 0 | 0 | 1 | 2 | 0 |
| `silphscope-digest.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `yq-error.yaml` | 1 / 1 | 0 | 0 | 3 | 0 | 0 |
| `yq-multi-error.yaml` | 0 / 1 | 1 | 0 | — | — | — |
| `sftp-promise.yaml` | 1 / 1 | 0 | 0 | 2 | 0 | 0 |
| `checkout-promise.yaml` | 0 / 2 | 2 | 0 | — | — | — |
| `redicloud-coroutines.yaml` | 0 / 1 | 1 | 0 | — | — | — |
| `sentry-async-executor.yaml` | 1 / 1 | 0 | 0 | 2 | 0 | 0 |

The yq multi-result case, previously scored at `9db6cce`, now reports
`result_use_path_unmodeled` for functions starting at lines 16 and 283 in the
before revision. It is incomplete, not evidence that the five labeled calls
were missed. The Kotlin case now reports `callback_execution_unresolved` at
line 120 of the before revision; unlike the earlier scored replay, it is
excluded from accuracy counts. The checkout cases still report unresolved
promise types, at two and 21 locations respectively. Replay stopped each
incomplete case at its before revision, so these observations say nothing
about its after revision. The Cilium mismatches remain the same intentional
cancellation handling reported in both snapshots, not independent defects.

These selected cases cover a small set of rule families and fix pairs. Results
are not a population-wide accuracy estimate; repeated findings within one fix
or across both snapshots are not independent samples. Reproduction and
dependency setup are documented in [the benchmark README](README.md).

A subsequent result-use change recognizes a returned Go result after a call
with a function-literal argument and an unconditional left-hand nil check in a
short-circuit expression. With that change, the yq multi-result replay completed
with five true positives and no false positives or false negatives. This is
still one selected fix pair; other callback and conditional shapes can remain
incomplete.

## Additional Kotlin and TSX cases

On Windows using the debug CLI from main `eac1f2ce5b59b07da0dad6b09741d239d9dd9d16`
and ast-grep 0.51.3, the following additional historical cases completed:

| Manifest | Completed / attempted | TP | FP | FN |
| --- | ---: | ---: | ---: | ---: |
| `redicloud-coroutines.yaml` | 1 / 1 | 2 | 2 | 0 |
| `sentry-async-executor.yaml` | 1 / 1 | 2 | 0 | 0 |

RediCloud's two false positives are the same registered callback in the two
revisions; replay exits unsuccessfully. The two Sentry positives share one fix
pair. Neither case establishes general language or rule accuracy. See the
[benchmark README](README.md) for labels and reproduction instructions.

## Earlier run: main 9db6cce

On Windows, the debug CLI built from main commit
`9db6ccea09881a5290d61cbe1db7cc8d0ed527a0` replayed the following pinned
manifests. Git was 2.56.0.windows.1, Rust 1.99.1, ast-grep 0.51.3,
Go 1.27.4, and Node 26.12.1. The Go probe was built from that same commit.
The TypeScript factory case used TypeScript 5.9.3; the experimental general
case used 5.6.3 with the documented legacy-catch adapter.

| Manifest | Completed / attempted | Incomplete | Failed | TP | FP | FN |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `azure-iot-matches.yaml` | 1 / 1 | 0 | 0 | 6 | 0 | 0 |
| `iceberg-go.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `jplag-trim.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `go-tools.yaml` | 1 / 1 | 0 | 0 | 0 | 0 | 0 |
| `uwas-rename.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `niverwizard-msw-delay.yaml` | 3 / 3 | 0 | 0 | 4 | 0 | 0 |
| `syzkaller-swallowed-error.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `cilium-swallowed-error.yaml` | 2 / 2 | 0 | 0 | 1 | 2 | 0 |
| `silphscope-digest.yaml` | 1 / 1 | 0 | 0 | 1 | 0 | 0 |
| `yq-error.yaml` | 1 / 1 | 0 | 0 | 3 | 0 | 0 |
| `yq-multi-error.yaml` | 1 / 1 | 0 | 0 | 5 | 0 | 0 |
| `sftp-promise.yaml` | 1 / 1 | 0 | 0 | 2 | 0 | 0 |
| `checkout-promise.yaml` | 0 / 2 | 2 | 0 | — | — | — |

The checkout cases remain unscored: replay reported unresolved type evidence
at two locations in one case and 21 in the other, and stopped each case at its
first incomplete revision. The Cilium replay exits unsuccessfully because the
same cancellation-handling line is reported in both snapshots. These are two
benchmark mismatches, not two independent code defects. The remaining rows
exited successfully. The first yq-error attempt was incomplete because its
offline Go package load failed; after fetching the pinned repository objects
and warming the isolated module/build caches, a fresh run completed as above.

The corpus at this revision includes one JavaScript fix,
but has no Kotlin or TSX historical cases and covers only a few rule
families. Multiple findings from one fix and repeated snapshots are not
independent examples. These results are not a population-wide accuracy
estimate, and absence of reported coverage gaps does not prove completeness.
The typed cases require the trusted compiler, probe, dependencies, and snapshot
setup documented in [the benchmark README](README.md).

## Earlier run

This is a small, selected baseline, not an estimate of population-wide rule
accuracy. The manifests link the original revisions, licenses, and label
rationales. A case compares two snapshots; a reported false positive in both
snapshots can describe the same unchanged source line twice.

Autoreview was built in debug mode from main commit
`406eae6093d9e287b233957ae2efa48a16e38fc7`, plus the checkout-scoped
`core.longpaths=true` change in this PR. The run used Windows, Git
2.56.0.windows.1, ast-grep 0.51.3, and Rust 1.99.1. The eight manifests below
were replayed against local clones containing their pinned commits. The Go and
TypeScript typed-probe suites were not part of this run and require the setup
described in the benchmark README. Scores count labeled findings, not projects.

| Manifest | Cases completed / attempted | TP | FP | FN |
| --- | ---: | ---: | ---: | ---: |
| `azure-iot-matches.yaml` | 1 / 1 | 6 | 0 | 0 |
| `iceberg-go.yaml` | 1 / 1 | 1 | 0 | 0 |
| `jplag-trim.yaml` | 1 / 1 | 1 | 0 | 0 |
| `go-tools.yaml` | 1 / 1 | 0 | 0 | 0 |
| `uwas-rename.yaml` | 1 / 1 | 1 | 0 | 0 |
| `niverwizard-msw-delay.yaml` | 3 / 3 | 4 | 0 | 0 |
| `syzkaller-swallowed-error.yaml` | 1 / 1 | 1 | 0 | 0 |
| `cilium-swallowed-error.yaml` | 2 / 2 | 1 | 2 | 0 |

The Cilium false positives are the same intentional cancellation handling
observed in both revisions. They remain real benchmark mismatches; the replay
exits unsuccessfully. The clean Go tools case has no positive labels, so it
cannot measure recall. The TypeScript cases share one historical fix and the
six Rust findings share one fix; they are not independent fix pairs. These
cases do not measure Kotlin, JavaScript, or TSX, nor most rule families.

Before the checkout change, syzkaller and both Cilium cases failed at Git
checkout with long-path errors and produced no scores. Afterward they reached
the analyzer on this machine. This does not establish that all Windows paths,
symlinks, external tools, or other repositories can be replayed successfully.

To reproduce a row, follow the clone and pinned-revision instructions in
[the benchmark README](README.md), build the desired Autoreview commit, put
ast-grep on `PATH` for ast-grep cases, and run from that repository:

```sh
autoreview rules replay --suite /absolute/path/to/autoreview/benchmarks/iceberg-go.yaml
```

Replace the suite with the manifest for the chosen repository. Keep the JSON
report and exit status: an incomplete or failed case is excluded from scoring,
and a completed case with unexpected or missing findings is still a mismatch.
