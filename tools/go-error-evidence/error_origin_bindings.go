package main

import (
	"go/ast"
	"go/token"
	"go/types"
)

// Bind only fresh, single-result locals; every subsequent use is checked below.
func forwardedBindings(body *ast.BlockStmt, param *types.Var, info *types.Info, summaries map[callee]*errorOrigin, unreachable map[ast.Node]bool) map[*types.Var]*ast.CallExpr {
	bindings := make(map[*types.Var]*ast.CallExpr)
	ast.Inspect(body, func(node ast.Node) bool {
		if node != nil && unreachable[node] {
			return false
		}
		assign, ok := node.(*ast.AssignStmt)
		if !ok || assign.Tok != token.DEFINE || len(assign.Lhs) != 1 || len(assign.Rhs) != 1 {
			return true
		}
		ident, ok := assign.Lhs[0].(*ast.Ident)
		if !ok {
			return true
		}
		variable, ok := info.Defs[ident].(*types.Var)
		if !ok || !types.Identical(variable.Type(), types.Universe.Lookup("error").Type()) {
			return true
		}
		call, ok := assign.Rhs[0].(*ast.CallExpr)
		if ok && isForwardedError(call, param, info, summaries) {
			bindings[variable] = call
		}
		return true
	})
	return bindings
}

func isBindingContext(ident *ast.Ident, stack []ast.Node, info *types.Info, summaries map[callee]*errorOrigin, bindings map[*types.Var]*ast.CallExpr) bool {
	call, parent := checkedContextCall(ident, stack, info, summaries)
	assign, ok := parent.(*ast.AssignStmt)
	if !ok || call == nil || len(assign.Lhs) != 1 || len(assign.Rhs) != 1 || assign.Rhs[0] != call {
		return false
	}
	name, ok := assign.Lhs[0].(*ast.Ident)
	if !ok {
		return false
	}
	variable, ok := info.Defs[name].(*types.Var)
	return ok && bindings[variable] == call
}

func isBoundError(expr ast.Expr, info *types.Info, bindings map[*types.Var]*ast.CallExpr) bool {
	ident, ok := expr.(*ast.Ident)
	if !ok {
		return false
	}
	variable, ok := info.Uses[ident].(*types.Var)
	return ok && bindings[variable] != nil
}

func isSafeErrorUse(ident *ast.Ident, parent ast.Node, info *types.Info) bool {
	switch node := parent.(type) {
	case *ast.ReturnStmt:
		return len(node.Results) == 1 && node.Results[0] == ident
	case *ast.BinaryExpr:
		if node.Op != token.EQL && node.Op != token.NEQ {
			return false
		}
		return node.X == ident && isPredeclaredNil(node.Y, info) || node.Y == ident && isPredeclaredNil(node.X, info)
	default:
		return false
	}
}
