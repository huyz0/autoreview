package main

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

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
