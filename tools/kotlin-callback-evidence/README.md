# Experimental Kotlin callback evidence

This standalone Kotlin compiler plugin explores how to identify a
resolved call receiving a directly written functional-interface conversion.
It does not classify callbacks as immediate or deferred or change finding
decisions. Autoreview can optionally attach matching snapshot records as
unverified diagnostic metadata, as described below.

Build with Python 3.10 or newer and an explicitly selected, trusted Kotlin
2.2.20 or 2.3.21 distribution and JDK. Build a separate plugin with the same
compiler version that will run it; the compiler plugin API changed between
these releases:

```sh
python3 tools/kotlin-callback-evidence/build.py \
  --kotlinc /path/to/kotlinc/bin/kotlinc \
  --compiler-jar /path/to/kotlinc/lib/kotlin-compiler.jar \
  --compiler-version 2.3.21 \
  --output /path/to/new-evidence-plugin.jar
```

The version option defaults to 2.2.20. Other compiler versions are rejected.

Choose a nonexistent output path for each invocation, then invoke that same
compiler with the source files, their required classpath and the plugin:

```sh
export AUTOREVIEW_CALLBACK_EVIDENCE_OUTPUT=/path/to/new-evidence.json
/path/to/kotlinc/bin/kotlinc /path/to/source.kt -cp /path/to/dependencies \
  -d /path/to/classes -Xplugin=/path/to/new-evidence-plugin.jar
```

On Windows use `kotlinc.bat` and set the environment variable in the shell.
Only use trusted compilers and inputs. This tool invokes the compiler; it is
not a sandbox for a malicious compiler, compiler plugins, or dependencies.

For an experimental copied-input run, use `snapshot.py` with every Kotlin
source and classpath JAR supplied explicitly, including the Kotlin standard
library. The output path must not already exist:

```sh
python3 tools/kotlin-callback-evidence/snapshot.py \
  --kotlinc /path/to/kotlinc/bin/kotlinc --plugin /path/to/evidence-plugin.jar \
  --jvm-target 21 \
  --source /path/to/source.kt --jar /path/to/kotlinc/lib/kotlin-stdlib.jar \
  --jar /path/to/dependency.jar --output /path/to/new-snapshot-report.json
```

This copies inputs into a temporary directory, disables the compiler's default
stdlib and reflection JARs, runs the compiler with a 120-second timeout, and
publishes evidence only after a successful exit and matching hashes for the
originals and copies. It maps evidence paths back to original source files.
Inputs are limited to `.kt` files and JARs, so callers must explicitly provide
the complete compilation inputs. It does not run a project build or discover
classpath configuration. Trusted compiler and plugin executables are still
required. The JDK and compiler installation are not snapshotted; before/after
hashes do not defend against adversarial mutation and restoration; and the
timeout does not guarantee termination of descendant processes. The snapshot
report is experimental.

`--jvm-target` defaults to `1.8`; set it to the project's target. The runner
validates the argument and records it in the report, but the selected compiler
still decides which target versions it supports. On Windows, long input paths
are opened with the extended path prefix. Other compiler options and build
plugins are not inferred or reproduced by the runner.

To attach matching records to unresolved Kotlin callback findings, set
`AUTOREVIEW_KOTLIN_CALLBACK_EVIDENCE` to the absolute path of a snapshot
report and `AUTOREVIEW_KOTLIN_CALLBACK_ROOT` to the absolute source checkout
root before running review. Report sources must be under that checkout root;
Autoreview maps their relative paths into the analyzed Git snapshot. It checks
the analyzed source hash and lambda span and records
the supplied callee and SAM type under `experimentalCallbackEvidence` in
finding metadata, marked `unverified`. It does not authenticate the report,
verify dependencies or runtime dispatch, or use the metadata to suppress a
finding. Missing, invalid, oversized or out-of-root reports produce a coverage
issue. A stale or unmatched record is not attached. In replay, a record can
only match a revision whose source bytes match; the metadata still does not
establish execution evidence or make an incomplete case scorable.

The JSON records the compiler version, source path and SHA-256, call and
lambda spans in UTF-16 code units, statically resolved callee name, regular
parameter index, SAM type name, declaration modality, and the dispatch
receiver's static classifier when available. A missing receiver is `null`.
The receiver type is diagnostic evidence only: an open method can dispatch to
a subclass with different callback behavior, and the record does not prove the
concrete runtime object. The call span is
compiler-provided and can omit a property receiver. The probe caps source
files at 512, individual files at 4 MiB, total source at 16 MiB, records at
20,000, and output at 1 MiB; exceeding a limit fails compilation. It refuses
to overwrite an existing output file.

Only direct SAM conversions represented in compiler IR as call arguments are
recorded. Absence of a record does not establish absence of a callback. A
callee name, parameter index, and SAM type do not uniquely identify an
overload or establish the runtime target under dynamic dispatch. Version 2
also records declared regular parameter classifiers in order; generic or
otherwise unrepresentable types are `null`. This deliberately omits receiver
type arguments, nullability, return types and module identity, so even
an all-non-null list is not a complete signature. The source
hash covers bytes read by the plugin, not necessarily the bytes previously
parsed by the compiler. Dependencies, classpaths and other source files are
not snapshot-verified in direct plugin mode. An output file can exist even if a later compiler
phase fails; never use it when the compiler exits unsuccessfully.

Before evidence can inform rule decisions, verify immutable source and
dependency snapshots, unambiguous declaration and overload identity, compiler
completion, bounded process execution, and a separately justified execution
contract. Unknown or incomplete cases must remain visible as coverage gaps.
See [the execution plan](../../docs/callback-execution-plan.md).

To run the integration tests, set `AUTOREVIEW_TEST_KOTLINC`,
`AUTOREVIEW_TEST_KOTLIN_COMPILER_JAR`, and `AUTOREVIEW_TEST_JAVAC` to absolute
paths for trusted tools. Set `AUTOREVIEW_TEST_KOTLIN_VERSION` to `2.3.21`
for that compiler; the default is `2.2.20`. Then run:

```sh
python3 tools/kotlin-callback-evidence/test_evidence.py
```

To include the Apache SSHD API regression, set `AUTOREVIEW_TEST_SSHD_CORE` and
`AUTOREVIEW_TEST_SSHD_COMMON` to trusted 2.17.1 JARs. CI downloads and checks
the pinned artifacts. The test compiles a small representative Kotlin source;
it does not build RediCloud or verify runtime dispatch or callback timing.
