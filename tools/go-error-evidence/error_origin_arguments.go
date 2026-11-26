package main

import (
	"go/ast"
	"go/constant"
	"go/token"
	"go/types"
)

// stableIntegerParameter rejects writes, address-taking, and closures before
// a call-site constant is used to reason about a function's branches.
func stableIntegerParameter(body *ast.BlockStmt, param *types.Var, info *types.Info) bool {
	basic, ok := param.Type().Underlying().(*types.Basic)
	if !ok || basic.Info()&types.IsInteger == 0 {
		return false
	}
	stable := true
	inspectWithStack(body, nil, func(node ast.Node, stack []ast.Node) {
		if _, ok := node.(*ast.FuncLit); ok {
			stable = false
		}
		ident, ok := node.(*ast.Ident)
		if ok && info.Uses[ident] == param && parameterMayChange(stack) {
			stable = false
		}
	})
	return stable
}

func parameterMayChange(stack []ast.Node) bool {
	if len(stack) < 2 {
		return false
	}
	ident := stack[len(stack)-1]
	for i := len(stack) - 2; i >= 0; i-- {
		changed := false
		switch node := stack[i].(type) {
		case *ast.ParenExpr:
			ident = node
			continue
		case *ast.UnaryExpr:
			changed = node.Op == token.AND && node.X == ident
		case *ast.IncDecStmt:
			changed = node.X == ident
		case *ast.AssignStmt:
			for _, lhs := range node.Lhs {
				changed = changed || lhs == ident
			}
		case *ast.RangeStmt:
			changed = node.Key == ident || node.Value == ident
		}
		return changed
	}
	return false
}

// integerGuard evaluates only comparisons between the identified parameter
// and a Go type-checked integer constant. Unsupported expressions are unknown.
func integerGuard(expr ast.Expr, param *types.Var, argument constant.Value, info *types.Info) (bool, bool) {
	if argument == nil || argument.Kind() != constant.Int {
		return false, false
	}
	condition, ok := expr.(*ast.BinaryExpr)
	if !ok || !comparisonOperator(condition.Op) || (!isIntegerParameter(condition.X, param, info) && !isIntegerParameter(condition.Y, param, info)) {
		return false, false
	}
	left, leftOK := integerOperand(condition.X, param, argument, info)
	right, rightOK := integerOperand(condition.Y, param, argument, info)
	if !leftOK || !rightOK {
		return false, false
	}
	return constant.Compare(left, condition.Op, right), true
}

func comparisonOperator(op token.Token) bool {
	return op == token.EQL || op == token.NEQ || op == token.LSS || op == token.LEQ || op == token.GTR || op == token.GEQ
}

func isIntegerParameter(expr ast.Expr, param *types.Var, info *types.Info) bool {
	ident, ok := expr.(*ast.Ident)
	return ok && info.Uses[ident] == param
}

func integerOperand(expr ast.Expr, param *types.Var, argument constant.Value, info *types.Info) (constant.Value, bool) {
	if ident, ok := expr.(*ast.Ident); ok && info.Uses[ident] == param {
		return argument, true
	}
	value := info.Types[expr].Value
	return value, value != nil && value.Kind() == constant.Int
}

type localFunction struct {
	decl     *ast.FuncDecl
	function *types.Func
}

func localFunctions(files []*ast.File, info *types.Info, enabled bool) map[callee]localFunction {
	result := make(map[callee]localFunction)
	if !enabled {
		return result
	}
	for _, file := range files {
		for _, item := range file.Decls {
			decl, function, identity := originDeclaration(item, info)
			if identity != nil {
				result[*identity] = localFunction{decl: decl, function: function}
			}
		}
	}
	return result
}

// specializeCall only reasons about a constant argument at this exact call;
// it never records the resulting fact as an unconditional function summary.
func specializeCall(call *ast.CallExpr, info *types.Info, summaries map[callee]*errorOrigin, functions map[callee]localFunction) *errorOrigin {
	target := resolvedCallee(call, info)
	if target == nil {
		return nil
	}
	local, ok := functions[*target]
	if !ok {
		return nil
	}
	signature := singleUnnamedError(local.function)
	if signature == nil {
		return nil
	}
	return specializeArguments(call, local.decl.Body, signature, info, summaries)
}

func specializeArguments(call *ast.CallExpr, body *ast.BlockStmt, signature *types.Signature, info *types.Info, summaries map[callee]*errorOrigin) *errorOrigin {
	offset := callReceiverOffset(call, info)
	for i := 0; i < signature.Params().Len(); i++ {
		if i+offset >= len(call.Args) {
			continue
		}
		argument := info.Types[call.Args[i+offset]].Value
		param := signature.Params().At(i)
		if argument == nil || argument.Kind() != constant.Int || !stableIntegerParameter(body, param, info) {
			continue
		}
		if origin := specializeContext(body, signature, param, argument, info, summaries); origin != nil {
			origin.Specialized = &integerEvidence{Parameter: i, Value: argument.ExactString()}
			return origin
		}
	}
	return nil
}

func specializeContext(body *ast.BlockStmt, signature *types.Signature, integer *types.Var, argument constant.Value, info *types.Info, summaries map[callee]*errorOrigin) *errorOrigin {
	unreachable := unreachableErrorBranches(body, info)
	matched := false
	ast.Inspect(body, func(node ast.Node) bool {
		branch, ok := node.(*ast.IfStmt)
		if ok {
			matched = markIntegerBranch(branch, integer, argument, info, unreachable) || matched
		}
		return true
	})
	if !matched {
		return nil
	}
	return contextOrigin(body, signature, info, summaries, unreachable)
}

func markIntegerBranch(branch *ast.IfStmt, integer *types.Var, argument constant.Value, info *types.Info, unreachable map[ast.Node]bool) bool {
	value, known := integerGuard(branch.Cond, integer, argument, info)
	if !known {
		return false
	}
	if value {
		if branch.Else != nil {
			unreachable[branch.Else] = true
		}
	} else {
		unreachable[branch.Body] = true
	}
	return true
}
