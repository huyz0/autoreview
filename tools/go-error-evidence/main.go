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
	Files           []sourceFile      `json:"files,omitempty"`
	Calls           []callSite        `json:"calls"`
	PackageDir      string            `json:"packageDir,omitempty"`
	GoBinary        string            `json:"goBinary,omitempty"`
	ExpectedSources map[string]string `json:"expectedSources,omitempty"`
}
type callResult struct {
	Path         string `json:"path"`
	Start        int    `json:"start"`
	Status       string `json:"status"`
	ResultCount  int    `json:"resultCount,omitempty"`
	ErrorIndexes []int  `json:"errorIndexes,omitempty"`
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
	config := types.Config{Importer: loaded.importer, Error: func(error) {}}
	files, byPath, reason := parseFiles(fset, loaded.files)
	if reason != "" {
		return incomplete(reason)
	}
	info := &types.Info{Types: make(map[ast.Expr]types.TypeAndValue)}
	if _, err := config.Check(loaded.path, fset, files, info); err != nil {
		return incomplete("type_errors")
	}
	indexes := indexCalls(fset, byPath)
	result := response{Version: 2, Status: "ok", Calls: make([]callResult, 0, len(req.Calls))}
	for _, call := range req.Calls {
		status, count, indexes := callStatus(indexes[call.Path][call.Start], info)
		result.Calls = append(result.Calls, callResult{Path: call.Path, Start: call.Start, Status: status, ResultCount: count, ErrorIndexes: indexes})
	}
	return result
}

func preparePackage(fset *token.FileSet, req request) (loadedPackage, string) {
	if req.PackageDir == "" && req.GoBinary == "" {
		return loadedPackage{files: req.Files, importer: importer.Default(), path: "autoreview.local/package"}, ""
	}
	if len(req.Files) != 0 {
		return loadedPackage{}, "invalid_request"
	}
	loaded, reason := loadPackage(fset, req.PackageDir, req.GoBinary)
	if reason == "" && !matchesExpectedSources(loaded.files, req.Calls, req.ExpectedSources) {
		return loadedPackage{}, "source_mismatch"
	}
	return loaded, reason
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
