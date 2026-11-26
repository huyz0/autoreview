# Experimental Kotlin callback evidence

This standalone Kotlin 2.2.20 compiler plugin explores how to identify a
resolved call receiving a directly written functional-interface conversion.
It is not integrated into Autoreview and does not alter findings. It does not
classify callbacks as immediate or deferred.

Build with Python 3.10 or newer and an explicitly selected, trusted Kotlin
2.2.20 distribution and JDK:

```sh
python3 tools/kotlin-callback-evidence/build.py \
  --kotlinc /path/to/kotlinc/bin/kotlinc \
  --compiler-jar /path/to/kotlinc/lib/kotlin-compiler.jar \
  --output /path/to/new-evidence-plugin.jar
```

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

The JSON records the compiler version, source path and SHA-256, call and
lambda spans in UTF-16 code units, statically resolved callee name, regular
parameter index, SAM type name, and declaration modality. The call span is
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
types, type arguments, nullability, return types and module identity, so even
an all-non-null list is not a complete signature. The source
hash covers bytes read by the plugin, not necessarily the bytes previously
parsed by the compiler. Dependencies, classpaths and other source files are
not snapshot-verified. An output file can exist even if a later compiler
phase fails; never use it when the compiler exits unsuccessfully.

Before evidence can inform rule decisions, verify immutable source and
dependency snapshots, unambiguous declaration and overload identity, compiler
completion, bounded process execution, and a separately justified execution
contract. Unknown or incomplete cases must remain visible as coverage gaps.
See [the execution plan](../../docs/callback-execution-plan.md).

To run the integration tests, set `AUTOREVIEW_TEST_KOTLINC`,
`AUTOREVIEW_TEST_KOTLIN_COMPILER_JAR`, and `AUTOREVIEW_TEST_JAVAC` to absolute
paths for trusted tools, then run:

```sh
python3 tools/kotlin-callback-evidence/test_evidence.py
```
