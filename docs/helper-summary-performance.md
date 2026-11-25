# Helper-summary performance check

This is a single local comparison, not a release benchmark. On Windows, two
development builds reviewed the same diff (`origin/main` at `7010adf` through
`b9cec8e`) with `--static-only --changed-lines --max-usd 0`. Both review-only
builds raised the cross-file source limit from 64 to 512; the shipped limit was
not changed. The same installed ast-grep, Go, golangci-lint, Clippy, and warm
build caches were used sequentially.

| Build | Wall time | Findings | Reported coverage limits |
| --- | ---: | ---: | ---: |
| Previous whole-program summary rounds | 177.3 s | 0 | 0 |
| Dependency worklist and no-source fast path | 75.8 s | 0 | 0 |

The timings include the entire static review, not only summary construction.
They are sensitive to machine load and cache state and do not establish a
general speedup. The differential tests compare the old fixed-point result
with the worklist on a helper chain and mutually recursive branches; the chain
also asserts that the worklist converges within one dependency-ordered pass.
