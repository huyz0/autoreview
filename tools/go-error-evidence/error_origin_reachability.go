package main

import (
	"go/ast"
	"go/constant"
	"go/types"
)

// Only Go compile-time constants establish reachability here. In particular,
// receiver fields and ordinary variables never become assumptions.
func unreachableErrorBranches(body *ast.BlockStmt, info *types.Info) map[ast.Node]bool {
	unreachable := make(map[ast.Node]bool)
	ast.Inspect(body, func(node ast.Node) bool {
		branch, ok := node.(*ast.IfStmt)
		if !ok {
			return true
		}
		value := info.Types[branch.Cond].Value
		if value == nil || value.Kind() != constant.Bool {
			return true
		}
		if constant.BoolVal(value) {
			if branch.Else != nil {
				unreachable[branch.Else] = true
			}
		} else {
			unreachable[branch.Body] = true
		}
		return true
	})
	return unreachable
}
