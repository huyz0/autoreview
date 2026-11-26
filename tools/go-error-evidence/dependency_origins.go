package main

import (
	"go/ast"
	"go/token"
	"go/types"
	"os"
	"path/filepath"
)

const dependencyLimit = 8
const dependencyBytesLimit = 16 * 1024 * 1024

// Only packages in go list's actual dependency graph and with independently
// supplied, matching build-selected source hashes may contribute summaries.
func verifyDependencySources(packages []listedPackage, target string, snapshots map[string]dependencySnapshot, exports map[string]string) ([]verifiedDependency, bool) {
	if len(snapshots) > dependencyLimit {
		return nil, false
	}
	var result []verifiedDependency
	total := 0
	for _, pkg := range packages {
		snapshot, wanted := snapshots[pkg.ImportPath]
		if !wanted {
			continue
		}
		if pkg.ImportPath == target || len(pkg.CgoFiles) != 0 || !sameDirectory(pkg.Dir, snapshot.Directory) {
			return nil, false
		}
		files, ok := readSources(pkg)
		if !ok || !matchesPackageSources(files, snapshot.Files, nil) {
			return nil, false
		}
		for _, file := range files {
			total += len(file.Source)
		}
		if total > dependencyBytesLimit {
			return nil, false
		}
		result = append(result, verifiedDependency{listed: pkg, files: files, exports: exports})
	}
	return result, len(result) == len(snapshots)
}

func sameDirectory(actual, expected string) bool {
	if !filepath.IsAbs(actual) || !filepath.IsAbs(expected) {
		return false
	}
	a, aErr := os.Stat(actual)
	b, bErr := os.Stat(expected)
	return aErr == nil && bErr == nil && a.IsDir() && b.IsDir() && os.SameFile(a, b)
}

func dependencyErrorOrigins(loaded loadedPackage, enabled bool) (map[callee]*errorOrigin, string) {
	result := make(map[callee]*errorOrigin)
	if !enabled {
		return result, ""
	}
	for _, dependency := range loaded.dependencies {
		files, info, ok := typecheckDependency(dependency)
		if !ok {
			return nil, "dependency_type_errors"
		}
		indexErrorOriginsInto(files, info, true, result)
	}
	return result, ""
}

func typecheckDependency(dependency verifiedDependency) ([]*ast.File, *types.Info, bool) {
	fset := token.NewFileSet()
	files, _, reason := parseFiles(fset, dependency.files)
	if reason != "" {
		return nil, nil, false
	}
	info := &types.Info{Types: make(map[ast.Expr]types.TypeAndValue), Uses: make(map[*ast.Ident]types.Object), Defs: make(map[*ast.Ident]types.Object), Selections: make(map[*ast.SelectorExpr]*types.Selection)}
	config := types.Config{Importer: packageImporter(fset, dependency.listed, dependency.exports), Error: func(error) {}}
	if _, err := config.Check(dependency.listed.ImportPath, fset, files, info); err != nil {
		return nil, nil, false
	}
	return files, info, true
}
