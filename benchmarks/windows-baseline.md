# Windows historical replay baseline

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
