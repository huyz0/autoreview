# Experimental Go error type evidence

This standalone prototype uses `go/types` to check whether specified call
expressions return exactly one predeclared `error`. It does not run Autoreview
or report discarded results. It has two input modes: caller-supplied source
files and experimental package loading. Both use JSON on standard input,
limited to 1 MiB.

In source-file mode the caller supplies every build-selected source file:

```json
{"files":[{"path":"main.go","source":"package p\nfunc f() error { return nil }\nfunc g() { f() }\n"}],"calls":[{"path":"main.go","start":51}]}
```

In package mode, provide an absolute package directory, an absolute trusted Go
binary, and SHA-256 hashes of every source file containing a requested call.
Call paths are package-relative file names:

```json
{"packageDir":"/project/pkg","goBinary":"/trusted/go/bin/go","expectedSources":{"main.go":"<sha256-of-analyzed-bytes>"},"calls":[{"path":"main.go","start":51}]}
```

Package mode runs `go list -deps -export -json .` with module downloads and
toolchain downloads disabled and `GOFLAGS` cleared. It uses the Go environment's
platform, workspace, and cache, applies default build tags, loads dependency
types, and reads the
selected ordinary Go files. Cached dependencies are required. It rejects a
package with cgo files and does not load test variants. Missing or mismatched
call-file hashes fail the whole request. A Go command and its build tools can
execute processes and read project configuration; use only trusted tools and
projects in an appropriately contained environment. The 60-second direct
child timeout does not yet contain descendant processes.

Each `start` is a zero-based UTF-8 byte offset of the call expression, not a
line or character number. Responses retain request order and report `error`
only when a callable has exactly one result identical to Go's predeclared
`error`; `not_proven` means a resolved call does not meet that condition, while
`unavailable` means the requested call was not found. Syntax, type-checking,
package-load, or source-identity failures produce an `incomplete` response.
Multi-result calls and `error` conversions are not classified as single-error
calls.

The caller must analyze an immutable package snapshot: other package files and
cached dependency exports can change during or after loading. Source-file mode
still relies on the caller to provide the complete build-selected package.
Before integration, explicit build tags, test variants, cgo, dependency and source snapshot identity,
process-tree containment, response limits, and coverage diagnostics need
explicit handling. Unavailable evidence must not be treated as clean.

With the pinned yq revision and its dependencies installed in an isolated
cache, package mode classified the three candidate calls as returning `error`.
That experiment does not run result-use analysis or count as a replay score.

Run the tests with a Go toolchain:

```sh
cd tools/go-error-evidence
GOPROXY=off GOSUMDB=off go test ./...
```
