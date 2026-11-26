package main

import (
	"go/ast"
	"go/types"
)

// This is evidence about explicit returned values, not about whether the
// caller should suppress them. Missing evidence always means unknown.
type errorOrigin struct {
	Kind             string           `json:"kind"`
	ContextParameter int              `json:"contextParameter"`
	Specialized      *integerEvidence `json:"specialized,omitempty"`
}

type integerEvidence struct {
	Parameter int    `json:"parameter"`
	Value     string `json:"value"`
}

const errorOriginPassLimit = 16

func indexErrorOrigins(files []*ast.File, info *types.Info, enabled bool) map[callee]*errorOrigin {
	result := make(map[callee]*errorOrigin)
	indexErrorOriginsInto(files, info, enabled, result)
	return result
}

func indexErrorOriginsInto(files []*ast.File, info *types.Info, enabled bool, result map[callee]*errorOrigin) {
	if !enabled {
		return
	}
	for pass := 0; pass < errorOriginPassLimit; pass++ {
		changed := false
		for _, file := range files {
			for _, item := range file.Decls {
				changed = indexErrorOrigin(item, info, result) || changed
			}
		}
		if !changed {
			break
		}
	}
}

func indexErrorOrigin(item ast.Decl, info *types.Info, result map[callee]*errorOrigin) bool {
	decl, function, identity := originDeclaration(item, info)
	if identity == nil || result[*identity] != nil {
		return false
	}
	if origin := summarizeContextReturns(decl, function, info, result); origin != nil {
		result[*identity] = origin
		return true
	}
	return false
}

func originDeclaration(item ast.Decl, info *types.Info) (*ast.FuncDecl, *types.Func, *callee) {
	decl, ok := item.(*ast.FuncDecl)
	if !ok || decl.Body == nil {
		return nil, nil, nil
	}
	function, ok := info.Defs[decl.Name].(*types.Func)
	if !ok {
		return nil, nil, nil
	}
	return decl, function, functionIdentity(function)
}

func summarizeContextReturns(decl *ast.FuncDecl, function *types.Func, info *types.Info, summaries map[callee]*errorOrigin) *errorOrigin {
	signature := singleUnnamedError(function)
	if signature == nil {
		return nil
	}
	return contextOrigin(decl.Body, signature, info, summaries, unreachableErrorBranches(decl.Body, info))
}

func singleUnnamedError(function *types.Func) *types.Signature {
	signature, ok := function.Type().(*types.Signature)
	if !ok || signature.Results().Len() != 1 {
		return nil
	}
	result := signature.Results().At(0)
	if result.Name() != "" || !types.Identical(result.Type(), types.Universe.Lookup("error").Type()) {
		return nil
	}
	return signature
}

func contextOrigin(body *ast.BlockStmt, signature *types.Signature, info *types.Info, summaries map[callee]*errorOrigin, unreachable map[ast.Node]bool) *errorOrigin {
	var found *errorOrigin
	for i := 0; i < signature.Params().Len(); i++ {
		param := signature.Params().At(i)
		if !isContextParameter(param) || !contextReturnsWithReachability(body, param, info, summaries, unreachable) {
			continue
		}
		if found != nil {
			return nil
		}
		found = &errorOrigin{Kind: "context_error_or_nil", ContextParameter: i}
	}
	return found
}

func isContextParameter(param *types.Var) bool {
	named, ok := types.Unalias(param.Type()).(*types.Named)
	return ok && named.Obj().Pkg() != nil && named.Obj().Pkg().Path() == "context" && named.Obj().Name() == "Context"
}

func inspectWithStack(root ast.Node, skip map[ast.Node]bool, visit func(ast.Node, []ast.Node)) {
	var stack []ast.Node
	ast.Inspect(root, func(node ast.Node) bool {
		if node == nil {
			stack = stack[:len(stack)-1]
			return true
		}
		if skip[node] {
			return false
		}
		stack = append(stack, node)
		visit(node, stack)
		return true
	})
}

func contextReturnsWithReachability(body *ast.BlockStmt, param *types.Var, info *types.Info, summaries map[callee]*errorOrigin, unreachable map[ast.Node]bool) bool {
	valid, sawError := true, false
	bindings := forwardedBindings(body, param, info, summaries, unreachable)
	inspectWithStack(body, unreachable, func(node ast.Node, stack []ast.Node) {
		var parent ast.Node
		if len(stack) > 1 {
			parent = stack[len(stack)-2]
		}
		switch item := node.(type) {
		case *ast.FuncLit, *ast.DeferStmt:
			valid = false
		case *ast.Ident:
			if info.Uses[item] == param {
				selector, ok := parent.(*ast.SelectorExpr)
				if (!ok || selector.X != item || (selector.Sel.Name != "Err" && selector.Sel.Name != "Done")) && !isForwardedContext(item, stack, info, summaries) && !isBindingContext(item, stack, info, summaries, bindings) {
					valid = false
				}
			}
			if variable, ok := info.Uses[item].(*types.Var); ok && bindings[variable] != nil && !isSafeErrorUse(item, parent, info) {
				valid = false
			}
		case *ast.ReturnStmt:
			if len(item.Results) != 1 {
				valid = false
				break
			}
			if isPredeclaredNil(item.Results[0], info) {
				break
			}
			if isContextError(item.Results[0], param, info) || isForwardedError(item.Results[0], param, info, summaries) || isBoundError(item.Results[0], info, bindings) {
				sawError = true
			} else {
				valid = false
			}
		}
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

func isForwardedContext(ident *ast.Ident, stack []ast.Node, info *types.Info, summaries map[callee]*errorOrigin) bool {
	call, parent := checkedContextCall(ident, stack, info, summaries)
	ret, ok := parent.(*ast.ReturnStmt)
	return ok && call != nil && len(ret.Results) == 1 && ret.Results[0] == call
}

func checkedContextCall(ident *ast.Ident, stack []ast.Node, info *types.Info, summaries map[callee]*errorOrigin) (*ast.CallExpr, ast.Node) {
	call, parent := enclosingCall(stack)
	if call == nil || !callPassesContext(call, ident, info, summaries) {
		return nil, nil
	}
	return call, parent
}

func callPassesContext(call *ast.CallExpr, ident *ast.Ident, info *types.Info, summaries map[callee]*errorOrigin) bool {
	index, ok := forwardedParameter(call, info, summaries)
	return ok && call.Args[index] == ident
}

func enclosingCall(stack []ast.Node) (*ast.CallExpr, ast.Node) {
	if len(stack) < 3 {
		return nil, nil
	}
	call, _ := stack[len(stack)-2].(*ast.CallExpr)
	return call, stack[len(stack)-3]
}

func isForwardedError(expr ast.Expr, param *types.Var, info *types.Info, summaries map[callee]*errorOrigin) bool {
	call, ok := expr.(*ast.CallExpr)
	if !ok {
		return false
	}
	index, ok := forwardedParameter(call, info, summaries)
	if !ok {
		return false
	}
	ident, ok := call.Args[index].(*ast.Ident)
	return ok && info.Uses[ident] == param
}

func forwardedParameter(call *ast.CallExpr, info *types.Info, summaries map[callee]*errorOrigin) (int, bool) {
	target := resolvedCallee(call, info)
	if target == nil {
		return 0, false
	}
	origin := summaries[*target]
	if origin == nil || origin.Kind != "context_error_or_nil" {
		return 0, false
	}
	index := origin.ContextParameter + callReceiverOffset(call, info)
	return index, index >= 0 && index < len(call.Args)
}

func callReceiverOffset(call *ast.CallExpr, info *types.Info) int {
	fun := call.Fun
	for {
		switch item := fun.(type) {
		case *ast.ParenExpr:
			fun = item.X
		case *ast.IndexExpr:
			fun = item.X
		case *ast.IndexListExpr:
			fun = item.X
		default:
			selector, ok := fun.(*ast.SelectorExpr)
			if ok {
				if selection := info.Selections[selector]; selection != nil && selection.Kind() == types.MethodExpr {
					return 1
				}
			}
			return 0
		}
	}
}
