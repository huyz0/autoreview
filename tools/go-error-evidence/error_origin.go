package main

import (
	"go/ast"
	"go/types"
)

// This is evidence about explicit returned values, not about whether the
// caller should suppress them. Missing evidence always means unknown.
type errorOrigin struct {
	Kind             string `json:"kind"`
	ContextParameter int    `json:"contextParameter"`
}

func indexErrorOrigins(files []*ast.File, info *types.Info, enabled bool) map[callee]*errorOrigin {
	result := make(map[callee]*errorOrigin)
	if !enabled {
		return result
	}
	for _, file := range files {
		for _, item := range file.Decls {
			decl, ok := item.(*ast.FuncDecl)
			if !ok || decl.Body == nil {
				continue
			}
			function, ok := info.Defs[decl.Name].(*types.Func)
			if !ok {
				continue
			}
			identity := functionIdentity(function)
			if identity == nil {
				continue
			}
			if origin := summarizeContextReturns(decl, function, info); origin != nil {
				result[*identity] = origin
			}
		}
	}
	return result
}

func summarizeContextReturns(decl *ast.FuncDecl, function *types.Func, info *types.Info) *errorOrigin {
	signature, ok := function.Type().(*types.Signature)
	if !ok || signature.Results().Len() != 1 {
		return nil
	}
	result := signature.Results().At(0)
	if result.Name() != "" || !types.Identical(result.Type(), types.Universe.Lookup("error").Type()) {
		return nil
	}
	var found *errorOrigin
	for i := 0; i < signature.Params().Len(); i++ {
		param := signature.Params().At(i)
		named, ok := types.Unalias(param.Type()).(*types.Named)
		if !ok || named.Obj().Pkg() == nil || named.Obj().Pkg().Path() != "context" || named.Obj().Name() != "Context" {
			continue
		}
		if contextReturnsOnly(decl.Body, param, info) {
			if found != nil {
				return nil
			}
			found = &errorOrigin{Kind: "context_error_or_nil", ContextParameter: i}
		}
	}
	return found
}

func contextReturnsOnly(body *ast.BlockStmt, param *types.Var, info *types.Info) bool {
	valid, sawError := true, false
	var stack []ast.Node
	ast.Inspect(body, func(node ast.Node) bool {
		if node == nil {
			stack = stack[:len(stack)-1]
			return true
		}
		var parent ast.Node
		if len(stack) > 0 {
			parent = stack[len(stack)-1]
		}
		stack = append(stack, node)
		switch item := node.(type) {
		case *ast.FuncLit, *ast.DeferStmt:
			valid = false
		case *ast.Ident:
			if info.Uses[item] == param {
				selector, ok := parent.(*ast.SelectorExpr)
				if !ok || selector.X != item || (selector.Sel.Name != "Err" && selector.Sel.Name != "Done") {
					valid = false
				}
			}
		case *ast.ReturnStmt:
			if len(item.Results) != 1 {
				valid = false
				break
			}
			if isPredeclaredNil(item.Results[0], info) {
				break
			}
			if isContextError(item.Results[0], param, info) {
				sawError = true
			} else {
				valid = false
			}
		}
		return true
	})
	return valid && sawError
}

func isPredeclaredNil(expr ast.Expr, info *types.Info) bool {
	ident, ok := expr.(*ast.Ident)
	return ok && info.Uses[ident] == types.Universe.Lookup("nil")
}

func isContextError(expr ast.Expr, param *types.Var, info *types.Info) bool {
	call, ok := expr.(*ast.CallExpr)
	if !ok || len(call.Args) != 0 {
		return false
	}
	selector, ok := call.Fun.(*ast.SelectorExpr)
	if !ok || selector.Sel.Name != "Err" {
		return false
	}
	ident, ok := selector.X.(*ast.Ident)
	return ok && info.Uses[ident] == param
}
