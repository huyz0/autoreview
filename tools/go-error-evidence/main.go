// go-error-evidence is an experimental, offline type-evidence probe.
// Its caller must provide the complete set of files selected by the Go build.
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"go/ast"
	"go/importer"
	"go/parser"
	"go/token"
	"go/types"
	"io"
	"os"
)

const maxInput = 1024 * 1024

type sourceFile struct {
	Path   string `json:"path"`
	Source string `json:"source"`
}
type callSite struct {
	Path  string `json:"path"`
	Start int    `json:"start"`
}
type request struct {
	Files               []sourceFile                  `json:"files,omitempty"`
	Calls               []callSite                    `json:"calls"`
	PackageDir          string                        `json:"packageDir,omitempty"`
	GoBinary            string                        `json:"goBinary,omitempty"`
	ExpectedSources     map[string]string             `json:"expectedSources,omitempty"`
	PackageSources      map[string]string             `json:"packageSources,omitempty"`
	ModuleMode          string                        `json:"moduleMode,omitempty"`
	IncludeCallees      bool                          `json:"includeCallees,omitempty"`
	IncludeErrorOrigins bool                          `json:"includeErrorOrigins,omitempty"`
	DependencySources   map[string]dependencySnapshot `json:"dependencySources,omitempty"`
}
type dependencySnapshot struct {
	Directory string            `json:"directory"`
	Files     map[string]string `json:"files"`
}
type callee struct {
	Package  string `json:"package"`
	Name     string `json:"name"`
	Receiver string `json:"receiver,omitempty"`
}
type callResult struct {
	Path         string       `json:"path"`
	Start        int          `json:"start"`
	Status       string       `json:"status"`
	ResultCount  int          `json:"resultCount,omitempty"`
	ErrorIndexes []int        `json:"errorIndexes,omitempty"`
	Callee       *callee      `json:"callee,omitempty"`
	ErrorOrigin  *errorOrigin `json:"errorOrigin,omitempty"`
}
type response struct {
	Version int          `json:"version"`
	Status  string       `json:"status"`
	Reason  string       `json:"reason,omitempty"`
	Calls   []callResult `json:"calls,omitempty"`
}

func analyze(req request) response {
	fset := token.NewFileSet()
	loaded, reason := preparePackage(fset, req)
	if reason != "" {
		return incomplete(reason)
	}
	files, byPath, info, reason := typecheckTarget(fset, loaded, req)
	if reason != "" {
		return incomplete(reason)
	}
	origins, reason := dependencyErrorOrigins(loaded, req.IncludeErrorOrigins)
	if reason != "" {
		return incomplete(reason)
	}
	indexes := indexCalls(fset, byPath)
	indexErrorOriginsInto(files, info, req.IncludeErrorOrigins, origins)
	analysis := callAnalysis{info: info, origins: origins, functions: localFunctions(files, info, req.IncludeErrorOrigins), request: req, remaining: 256}
	result := response{Version: 2, Status: "ok", Calls: make([]callResult, 0, len(req.Calls))}
	for _, call := range req.Calls {
		node := indexes[call.Path][call.Start]
		result.Calls = append(result.Calls, analysis.result(call, node))
	}
	return result
}

func typecheckTarget(fset *token.FileSet, loaded loadedPackage, req request) ([]*ast.File, map[string]*ast.File, *types.Info, string) {
	files, byPath, reason := parseFiles(fset, loaded.files)
	if reason != "" {
		return nil, nil, nil, reason
	}
	info := &types.Info{Types: make(map[ast.Expr]types.TypeAndValue)}
	if req.IncludeCallees || req.IncludeErrorOrigins {
		info.Uses = make(map[*ast.Ident]types.Object)
		info.Selections = make(map[*ast.SelectorExpr]*types.Selection)
		info.Defs = make(map[*ast.Ident]types.Object)
	}
	config := types.Config{Importer: loaded.importer, Error: func(error) {}}
	if _, err := config.Check(loaded.path, fset, files, info); err != nil {
		return nil, nil, nil, "type_errors"
	}
	return files, byPath, info, ""
}

type callAnalysis struct {
	info      *types.Info
	origins   map[callee]*errorOrigin
	functions map[callee]localFunction
	request   request
	remaining int
}

func (a *callAnalysis) result(call callSite, node *ast.CallExpr) callResult {
	status, count, indexes := callStatus(node, a.info)
	item := callResult{Path: call.Path, Start: call.Start, Status: status, ResultCount: count, ErrorIndexes: indexes}
	if a.request.IncludeCallees {
		item.Callee = resolvedCallee(node, a.info)
	}
	if a.request.IncludeErrorOrigins {
		if target := resolvedCallee(node, a.info); target != nil {
			item.ErrorOrigin = a.origins[*target]
			_, local := a.functions[*target]
			if item.ErrorOrigin == nil && local && a.remaining > 0 {
				a.remaining--
				item.ErrorOrigin = specializeCall(node, a.info, a.origins, a.functions)
			}
		}
	}
	return item
}

// resolvedCallee identifies only statically bound calls. Interface calls and
// function-valued variables can dispatch to an unknown implementation.
func resolvedCallee(call *ast.CallExpr, info *types.Info) *callee {
	if call == nil {
		return nil
	}
	return resolveFunctionExpr(call.Fun, info)
}

func resolveFunctionExpr(expr ast.Expr, info *types.Info) *callee {
	var object types.Object
	switch fun := expr.(type) {
	case *ast.ParenExpr:
		return resolveFunctionExpr(fun.X, info)
	case *ast.IndexExpr:
		return resolveFunctionExpr(fun.X, info)
	case *ast.IndexListExpr:
		return resolveFunctionExpr(fun.X, info)
	case *ast.Ident:
		object = info.Uses[fun]
	case *ast.SelectorExpr:
		if selection := info.Selections[fun]; selection != nil {
			recv := selection.Recv()
			if pointer, ok := recv.(*types.Pointer); ok {
				recv = pointer.Elem()
			}
			if _, ok := recv.Underlying().(*types.Interface); ok {
				return nil
			}
			object = selection.Obj()
		} else {
			object = info.Uses[fun.Sel]
		}
	default:
		return nil
	}
	function, ok := object.(*types.Func)
	if !ok || function.Pkg() == nil {
		return nil
	}
	return functionIdentity(function)
}

func functionIdentity(function *types.Func) *callee {
	if function == nil || function.Pkg() == nil {
		return nil
	}
	result := &callee{Package: function.Pkg().Path(), Name: function.Name()}
	if signature, ok := function.Type().(*types.Signature); ok && signature.Recv() != nil {
		receiver := signature.Recv().Type()
		if pointer, ok := receiver.(*types.Pointer); ok {
			receiver = pointer.Elem()
		}
		if _, ok := receiver.Underlying().(*types.Interface); ok {
			return nil
		}
		named, ok := receiver.(*types.Named)
		if !ok || named.Obj().Pkg() == nil {
			return nil
		}
		result.Receiver = named.Obj().Name()
	}
	return result
}

func preparePackage(fset *token.FileSet, req request) (loadedPackage, string) {
	if req.PackageDir == "" && req.GoBinary == "" {
		return prepareInlinePackage(req)
	}
	if len(req.Files) != 0 || len(req.DependencySources) > 0 && !req.IncludeErrorOrigins {
		return loadedPackage{}, "invalid_request"
	}
	loaded, reason := loadPackage(fset, req.PackageDir, req.GoBinary, req.ModuleMode, req.DependencySources)
	if reason == "" && (!matchesExpectedSources(loaded.files, req.Calls, req.ExpectedSources, false) ||
		(req.IncludeCallees || req.IncludeErrorOrigins) && !matchesPackageSources(loaded.files, req.PackageSources, req.ExpectedSources)) {
		return loadedPackage{}, "source_mismatch"
	}
	return loaded, reason
}

func prepareInlinePackage(req request) (loadedPackage, string) {
	if req.ModuleMode != "" || len(req.DependencySources) > 0 {
		return loadedPackage{}, "invalid_request"
	}
	return loadedPackage{files: req.Files, importer: importer.Default(), path: "autoreview.local/package"}, ""
}

func parseFiles(fset *token.FileSet, sources []sourceFile) ([]*ast.File, map[string]*ast.File, string) {
	if len(sources) == 0 {
		return nil, nil, "no_files"
	}
	files := make([]*ast.File, 0, len(sources))
	byPath := make(map[string]*ast.File, len(sources))
	for _, file := range sources {
		if file.Path == "" || byPath[file.Path] != nil {
			return nil, nil, "invalid_files"
		}
		parsed, err := parser.ParseFile(fset, file.Path, file.Source, 0)
		if err != nil {
			return nil, nil, "syntax_errors"
		}
		byPath[file.Path] = parsed
		files = append(files, parsed)
	}
	return files, byPath, ""
}

func indexCalls(fset *token.FileSet, files map[string]*ast.File) map[string]map[int]*ast.CallExpr {
	indexes := make(map[string]map[int]*ast.CallExpr, len(files))
	for path, file := range files {
		calls := make(map[int]*ast.CallExpr)
		ast.Inspect(file, func(node ast.Node) bool {
			if call, ok := node.(*ast.CallExpr); ok {
				calls[fset.Position(call.Pos()).Offset] = call
			}
			return true
		})
		indexes[path] = calls
	}
	return indexes
}

func callStatus(node *ast.CallExpr, info *types.Info) (string, int, []int) {
	if node == nil {
		return "unavailable", 0, nil
	}
	funType := info.TypeOf(node.Fun)
	if funType == nil {
		return "unavailable", 0, nil
	}
	signature, ok := funType.Underlying().(*types.Signature)
	if !ok {
		return "not_proven", 0, nil
	}
	return resultStatus(signature.Results())
}

func resultStatus(results *types.Tuple) (string, int, []int) {
	var errors []int
	for index := 0; index < results.Len(); index++ {
		if types.Identical(results.At(index).Type(), types.Universe.Lookup("error").Type()) {
			errors = append(errors, index)
		}
	}
	if results.Len() == 1 && len(errors) == 1 {
		return "error", 0, nil
	}
	if results.Len() > 1 && len(errors) > 0 {
		return "multi_error", results.Len(), errors
	}
	return "not_proven", 0, nil
}

func incomplete(reason string) response {
	return response{Version: 2, Status: "incomplete", Reason: reason}
}

func main() {
	data, err := io.ReadAll(io.LimitReader(os.Stdin, maxInput+1))
	if err != nil || len(data) > maxInput {
		fmt.Fprintln(os.Stderr, "invalid input")
		os.Exit(1)
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	var req request
	if decoder.Decode(&req) != nil || decoder.Decode(new(any)) != io.EOF {
		fmt.Fprintln(os.Stderr, "invalid request")
		os.Exit(1)
	}
	if err := json.NewEncoder(os.Stdout).Encode(analyze(req)); err != nil {
		os.Exit(1)
	}
}
