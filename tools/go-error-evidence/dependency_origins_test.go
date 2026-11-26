package main

import (
	"crypto/sha256"
	"encoding/hex"
	"go/token"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestDependencyOriginsAreIndependentOfPackageOrder(t *testing.T) {
	dir := t.TempDir()
	main := "package sample\nimport (\"context\"; \"example.test/cross/outer\")\nfunc use(ctx context.Context) { outer.Wrapper(ctx) }\n"
	outer := "package outer\nimport (\"context\"; \"example.test/cross/inner\")\nfunc Wrapper(ctx context.Context) error { return inner.Leaf(ctx) }\n"
	inner := "package inner\nimport \"context\"\nfunc Leaf(ctx context.Context) error { return ctx.Err() }\n"
	writeDependencyFiles(t, dir, map[string]string{"go.mod": "module example.test/cross\n\ngo 1.22\n", "main.go": main, "outer/outer.go": outer, "inner/inner.go": inner})
	snapshots := map[string]dependencySnapshot{
		"example.test/cross/outer": {Directory: filepath.Join(dir, "outer"), Files: map[string]string{"outer.go": digest(outer)}},
		"example.test/cross/inner": {Directory: filepath.Join(dir, "inner"), Files: map[string]string{"inner.go": digest(inner)}},
	}
	req := request{PackageDir: dir, GoBinary: goBinary(t), ExpectedSources: map[string]string{"main.go": digest(main)}, PackageSources: map[string]string{"main.go": digest(main)}, Calls: []callSite{{"main.go", strings.LastIndex(main, "outer.Wrapper(ctx)")}}, IncludeErrorOrigins: true, DependencySources: snapshots}
	if got := analyze(req); got.Status != "ok" || got.Calls[0].ErrorOrigin == nil || got.Calls[0].ErrorOrigin.ContextParameter != 0 {
		t.Fatalf("multi-hop call: %+v", got)
	}
	loaded, reason := loadPackage(token.NewFileSet(), dir, goBinary(t), "", snapshots)
	if reason != "" || len(loaded.dependencies) != 2 {
		t.Fatalf("load dependencies: %s, %d", reason, len(loaded.dependencies))
	}
	// Put the dependent package before its dependency, regardless of go list order.
	if loaded.dependencies[0].listed.ImportPath != "example.test/cross/outer" {
		loaded.dependencies[0], loaded.dependencies[1] = loaded.dependencies[1], loaded.dependencies[0]
	}
	origins, _, reason := dependencyErrorOrigins(loaded, true)
	if origin := origins[callee{Package: "example.test/cross/outer", Name: "Wrapper"}]; reason != "" || origin == nil || origin.ContextParameter != 0 {
		t.Fatalf("reversed dependencies: reason=%s origin=%+v", reason, origin)
	}
	loaded.dependencies = loaded.dependencies[:1]
	origins, _, reason = dependencyErrorOrigins(loaded, true)
	if origin := origins[callee{Package: "example.test/cross/outer", Name: "Wrapper"}]; reason != "" || origin != nil {
		t.Fatalf("unverified inner dependency: reason=%s origin=%+v", reason, origin)
	}
}

func TestVerifiedDependencyErrorOrigin(t *testing.T) {
	dir := t.TempDir()
	main := "package sample\nimport (\"context\"; \"example.test/cross/dep\")\nfunc wrapper(ctx context.Context) error { return dep.Leaf(ctx) }\nfunc use(ctx context.Context) { wrapper(ctx) }\n"
	dep := "package dep\nimport \"context\"\nfunc Leaf(ctx context.Context) error { return ctx.Err() }\n"
	writeDependencyFiles(t, dir, map[string]string{"go.mod": "module example.test/cross\n\ngo 1.22\n", "main.go": main, "dep/dep.go": dep})
	req := request{PackageDir: dir, GoBinary: goBinary(t), ExpectedSources: map[string]string{"main.go": digest(main)}, PackageSources: map[string]string{"main.go": digest(main)}, Calls: []callSite{{"main.go", strings.LastIndex(main, "wrapper(ctx)")}}, IncludeErrorOrigins: true}
	if got := analyze(req); got.Status != "ok" || got.Calls[0].ErrorOrigin != nil {
		t.Fatalf("unverified dependency: %+v", got)
	}
	req.DependencySources = map[string]dependencySnapshot{"example.test/cross/dep": {Directory: filepath.Join(dir, "dep"), Files: map[string]string{"dep.go": digest(dep)}}}
	got := analyze(req)
	if got.Status != "ok" || got.Calls[0].ErrorOrigin == nil || got.Calls[0].ErrorOrigin.ContextParameter != 0 {
		t.Fatalf("verified dependency: %+v", got)
	}
	req.DependencySources["example.test/cross/dep"] = dependencySnapshot{Directory: filepath.Join(dir, "dep"), Files: map[string]string{"dep.go": strings.Repeat("0", 64)}}
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "dependency_source_mismatch" {
		t.Fatalf("stale dependency: %+v", got)
	}
	unsafeDep := "package dep\nimport (\"context\"; \"errors\")\nfunc Leaf(ctx context.Context) error { if ctx.Err() != nil { return ctx.Err() }; return errors.New(\"other\") }\n"
	writeDependencyFiles(t, dir, map[string]string{"dep/dep.go": unsafeDep})
	req.DependencySources["example.test/cross/dep"] = dependencySnapshot{Directory: filepath.Join(dir, "dep"), Files: map[string]string{"dep.go": digest(unsafeDep)}}
	if got := analyze(req); got.Status != "ok" || got.Calls[0].ErrorOrigin != nil {
		t.Fatalf("other dependency error: %+v", got)
	}
	req.DependencySources["example.test/cross/dep"] = dependencySnapshot{Directory: dir, Files: map[string]string{"dep.go": digest(unsafeDep)}}
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "dependency_source_mismatch" {
		t.Fatalf("wrong dependency directory: %+v", got)
	}
}

func TestVerifiedDependencyConstantArguments(t *testing.T) {
	dir := t.TempDir()
	main := `package sample
import ("context"; "example.test/cross/dep")
func use(ctx context.Context, n int, w *dep.Worker) {
 dep.Guarded(ctx, 1)
 dep.Guarded(ctx, 2)
 dep.Guarded(ctx, n)
 dep.Mutated(ctx, 1)
 dep.Escaped(ctx, 1)
 w.Guarded(ctx, 1)
 ((*dep.Worker).Guarded)(w, ctx, 1)
}
`
	dep := `package dep
import ("context"; "errors")
func Guarded(ctx context.Context, n int) error {
 if n > 1 { return errors.New("other") }; return ctx.Err()
}
func Mutated(ctx context.Context, n int) error {
 n++; if n > 1 { return errors.New("other") }; return ctx.Err()
}
func Escaped(ctx context.Context, n int) error {
 _ = &n; if n > 1 { return errors.New("other") }; return ctx.Err()
}
type Worker struct{}
func (*Worker) Guarded(ctx context.Context, n int) error {
 if n > 1 { return errors.New("other") }; return ctx.Err()
}
`
	writeDependencyFiles(t, dir, map[string]string{"go.mod": "module example.test/cross\n\ngo 1.22\n", "main.go": main, "dep/dep.go": dep})
	var calls []callSite
	for _, expression := range []string{"dep.Guarded(ctx, 1)", "dep.Guarded(ctx, 2)", "dep.Guarded(ctx, n)", "dep.Mutated(ctx, 1)", "dep.Escaped(ctx, 1)", "w.Guarded(ctx, 1)", "((*dep.Worker).Guarded)(w, ctx, 1)"} {
		calls = append(calls, callSite{"main.go", strings.Index(main, expression)})
	}
	req := request{PackageDir: dir, GoBinary: goBinary(t), ExpectedSources: map[string]string{"main.go": digest(main)}, PackageSources: map[string]string{"main.go": digest(main)}, Calls: calls, IncludeErrorOrigins: true}
	if got := analyze(req); got.Status != "ok" || got.Calls[0].ErrorOrigin != nil {
		t.Fatalf("unverified dependency: %+v", got)
	}
	req.DependencySources = map[string]dependencySnapshot{"example.test/cross/dep": {Directory: filepath.Join(dir, "dep"), Files: map[string]string{"dep.go": digest(dep)}}}
	got := analyze(req)
	if got.Status != "ok" || len(got.Calls) != len(calls) {
		t.Fatalf("analysis: %+v", got)
	}
	for i, call := range got.Calls {
		want := i == 0 || i == 5 || i == 6
		if (call.ErrorOrigin != nil) != want {
			t.Errorf("call %d: origin=%+v, want evidence %v", i, call.ErrorOrigin, want)
			continue
		}
		if want && (call.ErrorOrigin.ContextParameter != 0 || call.ErrorOrigin.Specialized == nil || call.ErrorOrigin.Specialized.Parameter != 1 || call.ErrorOrigin.Specialized.Value != "1") {
			t.Errorf("call %d: unexpected origin %+v", i, call.ErrorOrigin)
		}
	}
	req.DependencySources["example.test/cross/dep"] = dependencySnapshot{Directory: filepath.Join(dir, "dep"), Files: map[string]string{"dep.go": strings.Repeat("0", 64)}}
	if got := analyze(req); got.Status != "incomplete" || got.Reason != "dependency_source_mismatch" {
		t.Fatalf("stale dependency: %+v", got)
	}
}

func writeDependencyFiles(t *testing.T, dir string, files map[string]string) {
	t.Helper()
	for name, content := range files {
		path := filepath.Join(dir, name)
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0644); err != nil {
			t.Fatal(err)
		}
	}
}

func digest(source string) string {
	sum := sha256.Sum256([]byte(source))
	return hex.EncodeToString(sum[:])
}
