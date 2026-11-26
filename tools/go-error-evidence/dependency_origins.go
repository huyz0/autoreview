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

type checkedDependency struct {
	files []*ast.File
	info  *types.Info
}

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
		dependency, size, ok := verifyDependency(pkg, target, snapshot, exports)
		if !ok || total+size > dependencyBytesLimit {
			return nil, false
		}
		total += size
		result = append(result, dependency)
	}
	return result, len(result) == len(snapshots)
}

func verifyDependency(pkg listedPackage, target string, snapshot dependencySnapshot, exports map[string]string) (verifiedDependency, int, bool) {
	if pkg.ImportPath == target || len(pkg.CgoFiles) != 0 || !sameDirectory(pkg.Dir, snapshot.Directory) {
		return verifiedDependency{}, 0, false
	}
	files, ok := readSources(pkg)
	if !ok || !matchesPackageSources(files, snapshot.Files, nil) {
		return verifiedDependency{}, 0, false
	}
	total := 0
	for _, file := range files {
		total += len(file.Source)
	}
	return verifiedDependency{listed: pkg, files: files, exports: exports}, total, true
}

func sameDirectory(actual, expected string) bool {
	if !filepath.IsAbs(actual) || !filepath.IsAbs(expected) {
		return false
	}
	a, aErr := os.Stat(actual)
	b, bErr := os.Stat(expected)
	return aErr == nil && bErr == nil && a.IsDir() && b.IsDir() && os.SameFile(a, b)
}

func dependencyErrorOrigins(loaded loadedPackage, enabled bool) (map[callee]*errorOrigin, map[callee]localFunction, string) {
	result := make(map[callee]*errorOrigin)
	functions := make(map[callee]localFunction)
	if !enabled {
		return result, functions, ""
	}
	checked, ok := checkDependencies(loaded.dependencies, functions)
	if !ok {
		return nil, nil, "dependency_type_errors"
	}
	indexDependencyOrigins(checked, result)
	return result, functions, ""
}

func checkDependencies(dependencies []verifiedDependency, functions map[callee]localFunction) ([]checkedDependency, bool) {
	checked := make([]checkedDependency, 0, len(dependencies))
	for _, dependency := range dependencies {
		files, info, ok := typecheckDependency(dependency)
		if !ok {
			return nil, false
		}
		checked = append(checked, checkedDependency{files: files, info: info})
		for identity, function := range localFunctions(files, info, true) {
			functions[identity] = function
		}
	}
	return checked, true
}

func indexDependencyOrigins(checked []checkedDependency, result map[callee]*errorOrigin) {
	// Dependencies may be supplied in any order. Revisit verified packages
	// until summaries stop changing, with the same bound as local helpers.
	for pass := 0; pass < errorOriginPassLimit; pass++ {
		before := len(result)
		for _, dependency := range checked {
			indexErrorOriginsInto(dependency.files, dependency.info, true, result)
		}
		if len(result) == before {
			break
		}
	}
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
