# Experimental Go error type evidence

This experimental probe uses `go/types` to check whether specified calls
return exactly one predeclared `error`. Autoreview can invoke package mode when
explicitly configured with trusted probe and Go executable paths; the probe
itself does not decide whether a result is discarded. It has two input modes:
caller-supplied source files and experimental package loading. Both use JSON on
standard input, limited to 1 MiB.

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
toolchain downloads disabled and `GOFLAGS=-mod=readonly`. It uses the Go environment's
platform, workspace, and cache, applies default build tags, loads dependency
types, and reads the
selected ordinary Go files. Cached dependencies are required. It rejects a
package with cgo files and does not load test variants. Missing call-file hashes or mismatches for any supplied source hash fail the
whole request. A Go command and its build tools can
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
Explicit build tags, test variants, cgo, full dependency and source snapshot
identity, and process-tree containment remain unsupported. Autoreview bounds
requests and responses and reports unavailable evidence as a coverage limit;
it does not treat that evidence as a clean result.

With the pinned yq revision and its dependencies installed in an isolated
cache, package mode classified the three candidate calls as returning `error`.
The integrated experimental rule's historical replay also finds the three
labeled discarded results and no findings after the fix. This single fix pair
is not an estimate of general precision or recall.

Run the tests with a Go toolchain:

```sh
cd tools/go-error-evidence
GOPROXY=off GOSUMDB=off go test ./...
```
