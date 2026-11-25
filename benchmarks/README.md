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
