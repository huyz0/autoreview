// Prototype: read one JSON request from stdin and write one JSON response.
// The caller must provide the TypeScript package path explicitly.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

function run(request) {
  let ts;
  try {
    ts = require(request.typescriptModule);
  } catch (error) {
    // TypeScript 7's native package does not expose the legacy CommonJS
    // compiler API at its package root. Do not execute a different compiler.
    try {
      const pkg = JSON.parse(fs.readFileSync(path.join(request.typescriptModule, 'package.json'), 'utf8'));
      if (pkg.name === 'typescript' && Number.parseInt(pkg.version, 10) >= 7) {
        return { status: 'incomplete', reason: 'compiler_api_unsupported' };
      }
    } catch (_) { throw error; }
    throw error;
  }
  if (!ts || typeof ts.readConfigFile !== 'function' || typeof ts.parseJsonConfigFileContent !== 'function' ||
      typeof ts.createProgram !== 'function' || typeof ts.getPreEmitDiagnostics !== 'function' ||
      typeof ts.isCallExpression !== 'function' || typeof ts.isAwaitExpression !== 'function' ||
      typeof ts.isParenthesizedExpression !== 'function' || typeof ts.isFunctionLike !== 'function' ||
      typeof ts.isReturnStatement !== 'function' || typeof ts.isArrowFunction !== 'function' ||
      typeof ts.isFunctionExpression !== 'function' || typeof ts.isVoidExpression !== 'function' ||
      typeof ts.isBlock !== 'function' || typeof ts.isVariableDeclarationList !== 'function' ||
      typeof ts.isVariableDeclaration !== 'function' || typeof ts.isVariableStatement !== 'function' ||
      typeof ts.isExpressionStatement !== 'function' || typeof ts.isIdentifier !== 'function' || typeof ts.forEachChild !== 'function') {
    return { status: 'incomplete', reason: 'compiler_api_unsupported' };
  }
  const config = ts.readConfigFile(request.config, ts.sys.readFile);
  if (config.error) return { status: 'incomplete', reason: 'config_unreadable' };
  const parsed = ts.parseJsonConfigFileContent(config.config, ts.sys, path.dirname(request.config), undefined, request.config);
  if (parsed.errors.length) return { status: 'incomplete', reason: 'config_invalid' };
  const program = ts.createProgram(parsed.fileNames, { ...parsed.options, noEmit: true });
  if (ts.getPreEmitDiagnostics(program).some(diagnostic => diagnostic.category === ts.DiagnosticCategory.Error)) {
    return { status: 'incomplete', reason: 'project_type_errors' };
  }
  const checker = program.getTypeChecker();
  if (typeof checker.getAwaitedType !== 'function' || typeof checker.getPropertyOfType !== 'function' ||
      typeof checker.getSymbolAtLocation !== 'function') {
    return { status: 'incomplete', reason: 'compiler_api_unsupported' };
  }
  function promiseLike(type) {
    if (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.Never)) return false;
    if (type.isUnion()) return type.types.every(promiseLike);
    if (!(type.flags & ts.TypeFlags.Object) || !checker.getPropertyOfType(type, 'then')) return false;
    const awaited = checker.getAwaitedType(type);
    return !!awaited && awaited !== type;
  }
  function definitelyNotPromise(type) {
    if (type.isUnion()) return type.types.every(definitelyNotPromise);
    // A function typed as returning void may actually return a promise:
    // TypeScript permits async functions in void-returning callback positions.
    const primitive = ts.TypeFlags.StringLike | ts.TypeFlags.NumberLike | ts.TypeFlags.BooleanLike |
      ts.TypeFlags.BigIntLike | ts.TypeFlags.Null | ts.TypeFlags.Undefined |
      ts.TypeFlags.ESSymbolLike;
    return !!(type.flags & primitive) && !checker.getPropertyOfType(type, 'then');
  }
  // Follow only expression positions whose evaluated value becomes the value
  // of the containing expression. Conditions and left operands are inputs,
  // not necessarily the value adopted by await or an async return.
  function adoptedExpression(node) {
    let value = node;
    while (value.parent) {
      const parent = value.parent;
      if (ts.isParenthesizedExpression(parent) && parent.expression === value ||
          parent.kind === ts.SyntaxKind.ConditionalExpression && (parent.whenTrue === value || parent.whenFalse === value) ||
          parent.kind === ts.SyntaxKind.BinaryExpression && parent.right === value &&
            [ts.SyntaxKind.AmpersandAmpersandToken, ts.SyntaxKind.BarBarToken, ts.SyntaxKind.QuestionQuestionToken].includes(parent.operatorToken.kind)) {
        value = parent;
      } else break;
    }
    return value;
  }
  function directlyAwaited(node) {
    const value = adoptedExpression(node);
    return !!value.parent && ts.isAwaitExpression(value.parent) && value.parent.expression === value;
  }
  // Follow only consecutive immutable aliases to an await, with no intervening
  // operation that could prevent the returned value from being observed.
  function immediatelyAwaitedBinding(node) {
    let value = node;
    while (value.parent && ts.isParenthesizedExpression(value.parent) && value.parent.expression === value) value = value.parent;
    const declaration = value.parent;
    if (!declaration || !ts.isVariableDeclaration(declaration) || declaration.initializer !== value || !ts.isIdentifier(declaration.name)) return false;
    const list = declaration.parent;
    if (!list || !ts.isVariableDeclarationList(list) || !ordinaryConst(list)) return false;
    const statement = list.parent;
    if (!statement || !ts.isVariableStatement(statement) || !statement.parent?.statements) return false;
    const statements = statement.parent.statements;
    const position = statements.indexOf(statement);
    if (position < 0) return false;
    let binding = checker.getSymbolAtLocation(declaration.name);
    if (!binding) return false;
    for (let index = position + 1, steps = 0; steps < 32 && index < statements.length; index++, steps++) {
      const next = statements[index];
      if (ts.isExpressionStatement(next) && ts.isAwaitExpression(next.expression)) {
        return sameBinding(next.expression.expression, binding);
      }
      if (!ts.isVariableStatement(next) || !ordinaryConst(next.declarationList)) return false;
      const alias = next.declarationList.declarations[0];
      if (!ts.isIdentifier(alias.name) || !alias.initializer || !sameBinding(alias.initializer, binding)) return false;
      binding = checker.getSymbolAtLocation(alias.name);
      if (!binding) return false;
    }
    return false;
  }
  function sameBinding(node, binding) {
    while (ts.isParenthesizedExpression(node)) node = node.expression;
    return ts.isIdentifier(node) && checker.getSymbolAtLocation(node) === binding;
  }
  function ordinaryConst(list) {
    return !!(list.flags & ts.NodeFlags.Const) && !(list.flags & (ts.NodeFlags.Using || 0)) && list.declarations.length === 1;
  }
  // Only a directly called, immutable local function literal can prove its
  // actual return here. A void signature alone permits async implementations.
  const localReturnCache = new WeakMap();
  function localFunctionLiteral(call) {
    if (!ts.isIdentifier(call.expression) || call.questionDotToken) return null;
    const symbol = checker.getSymbolAtLocation(call.expression);
    if (!symbol || symbol.declarations?.length !== 1) return null;
    const declaration = symbol.declarations[0];
    if (!ts.isVariableDeclaration(declaration) || declaration.getSourceFile() !== call.getSourceFile() ||
        !ts.isVariableDeclarationList(declaration.parent) || !ordinaryConst(declaration.parent)) return null;
    const fn = declaration.initializer;
    if (!fn || !(ts.isArrowFunction(fn) || ts.isFunctionExpression(fn)) || !fn.body ||
        fn.asteriskToken) return null;
    return fn;
  }
  function localReturnsNoValue(fn) {
    if (!fn || asyncFunction(fn)) return false;
    if (localReturnCache.has(fn)) return localReturnCache.get(fn);
    if (!ts.isBlock(fn.body)) return ts.isVoidExpression(fn.body);
    const pending = [fn.body];
    let remaining = 10000;
    let safe = true;
    while (pending.length && safe) {
      if (--remaining < 0) { safe = false; break; }
      const node = pending.pop();
      if (node !== fn.body && ts.isFunctionLike(node)) continue;
      if (ts.isReturnStatement(node) && node.expression && !ts.isVoidExpression(node.expression)) {
        safe = false;
      } else ts.forEachChild(node, child => { pending.push(child); });
    }
    localReturnCache.set(fn, safe);
    return safe;
  }
  // Only traverse ordinary blocks, ifs and switch cases: finally blocks,
  // iterator cleanup and resource disposal can replace a pending return.
  function returnedFromAsync(node) {
    const value = adoptedExpression(node);
    let current = value.parent;
    if (current && ts.isArrowFunction(current) && current.body === value) return asyncFunction(current);
    if (!current || !ts.isReturnStatement(current) || current.expression !== value) return false;
    current = current.parent;
    while (current) {
      const fn = current.parent;
      if (fn && ts.isBlock(current) && ts.isFunctionLike(fn) && fn.body === current) {
        return asyncFunction(fn) && !hasUsing(current);
      }
      if (![ts.SyntaxKind.Block, ts.SyntaxKind.IfStatement, ts.SyntaxKind.SwitchStatement,
        ts.SyntaxKind.CaseBlock, ts.SyntaxKind.CaseClause, ts.SyntaxKind.DefaultClause].includes(current.kind)) return false;
      current = current.parent;
    }
    return false;
  }
  function asyncFunction(node) {
    return !node.asteriskToken && !!node.modifiers?.some(modifier => modifier.kind === ts.SyntaxKind.AsyncKeyword);
  }
  const usingCache = new WeakMap();
  function hasUsing(body) {
    if (usingCache.has(body)) return usingCache.get(body);
    let found = false;
    function visit(node) {
      if (found || (node !== body && ts.isFunctionLike(node))) return;
      if (ts.isVariableDeclarationList(node) && (node.flags & (ts.NodeFlags.Using || 0))) found = true;
      else ts.forEachChild(node, visit);
    }
    visit(body);
    usingCache.set(body, found);
    return found;
  }
  const files = new Map();
  function indexCalls(file) {
    const nodes = [];
    function visit(node) {
      if (ts.isCallExpression(node)) nodes.push({ node, start: node.getStart(file), order: nodes.length });
      ts.forEachChild(node, visit);
    }
    visit(file);
    // Accumulate UTF-8 offsets in source order instead of encoding the whole
    // prefix for every call. Multiple calls can share a start offset; the
    // request cannot identify which one was lowered, so leave it unresolved.
    nodes.sort((a, b) => a.start - b.start || a.order - b.order);
    const index = new Map();
    let previous = 0;
    let bytes = 0;
    for (const entry of nodes) {
      bytes += Buffer.byteLength(file.text.slice(previous, entry.start), 'utf8');
      index.set(bytes, index.has(bytes) ? null : entry.node);
      previous = entry.start;
    }
    return index;
  }
  const calls = request.calls.map(call => {
    const file = program.getSourceFile(call.path);
    if (!file) return { ...call, status: 'unavailable' };
    if (!files.has(file)) {
      files.set(file, { digest: crypto.createHash('sha256').update(file.text, 'utf8').digest('hex'), calls: null });
    }
    const cached = files.get(file);
    if (cached.digest !== call.source_sha256) return { ...call, status: 'source_changed' };
    if (!cached.calls) cached.calls = indexCalls(file);
    const selected = cached.calls.get(call.start);
    if (selected === null) return { ...call, status: 'not_proven' };
    if (!selected) return { ...call, status: 'unavailable' };
    const type = checker.getTypeAtLocation(selected);
    const local = localFunctionLiteral(selected);
    return { ...call, status: promiseLike(type) || local && asyncFunction(local) ? 'promise_like' : definitelyNotPromise(type) || localReturnsNoValue(local) ? 'not_promise' : directlyAwaited(selected) || immediatelyAwaitedBinding(selected) || returnedFromAsync(selected) ? 'observed' : 'not_proven' };
  });
  return { status: 'ok', calls };
}

try {
  const request = JSON.parse(fs.readFileSync(0, 'utf8'));
  process.stdout.write(JSON.stringify(run(request)));
} catch (_) {
  process.stdout.write(JSON.stringify({ status: 'incomplete', reason: 'checker_unavailable' }));
}
