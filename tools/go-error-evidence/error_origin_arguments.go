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
	var stack []ast.Node
	ast.Inspect(body, func(node ast.Node) bool {
		if node == nil {
			stack = stack[:len(stack)-1]
			return true
		}
		stack = append(stack, node)
		if _, ok := node.(*ast.FuncLit); ok {
			stable = false
		}
		ident, ok := node.(*ast.Ident)
		if ok && info.Uses[ident] == param && parameterMayChange(stack) {
			stable = false
		}
		return true
	})
	return stable
}

func parameterMayChange(stack []ast.Node) bool {
	if len(stack) < 2 {
		return false
	}
	ident := stack[len(stack)-1]
	for i := len(stack) - 2; i >= 0; i-- {
		switch node := stack[i].(type) {
		case *ast.ParenExpr:
			ident = node
		case *ast.UnaryExpr:
			return node.Op == token.AND && node.X == ident
		case *ast.IncDecStmt:
			return node.X == ident
		case *ast.AssignStmt:
			for _, lhs := range node.Lhs {
				if lhs == ident {
					return true
				}
			}
			return false
		case *ast.RangeStmt:
			return node.Key == ident || node.Value == ident
		default:
			return false
		}
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
	if !ok {
		return false, false
	}
	if !isIntegerParameter(condition.X, param, info) && !isIntegerParameter(condition.Y, param, info) {
		return false, false
	}
	if condition.Op != token.EQL && condition.Op != token.NEQ && condition.Op != token.LSS && condition.Op != token.LEQ && condition.Op != token.GTR && condition.Op != token.GEQ {
		return false, false
	}
	left, leftOK := integerOperand(condition.X, param, argument, info)
	right, rightOK := integerOperand(condition.Y, param, argument, info)
	if !leftOK || !rightOK {
		return false, false
	}
	return constant.Compare(left, condition.Op, right), true
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
	signature, ok := local.function.Type().(*types.Signature)
	if !ok || signature.Results().Len() != 1 {
		return nil
	}
	output := signature.Results().At(0)
	if output.Name() != "" || !types.Identical(output.Type(), types.Universe.Lookup("error").Type()) {
		return nil
	}
	offset := callReceiverOffset(call, info)
	for i := 0; i < signature.Params().Len(); i++ {
		if i+offset >= len(call.Args) {
			continue
		}
		argument := info.Types[call.Args[i+offset]].Value
		param := signature.Params().At(i)
		if argument == nil || argument.Kind() != constant.Int || !stableIntegerParameter(local.decl.Body, param, info) {
			continue
		}
		if origin := specializeContext(local.decl.Body, signature, param, argument, info, summaries); origin != nil {
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
		if !ok {
			return true
		}
		value, known := integerGuard(branch.Cond, integer, argument, info)
		if !known {
			return true
		}
		matched = true
		if value {
			if branch.Else != nil {
				unreachable[branch.Else] = true
			}
		} else {
			unreachable[branch.Body] = true
		}
		return true
	})
	if !matched {
		return nil
	}
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
