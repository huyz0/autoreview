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
      typeof ts.isBlock !== 'function' || typeof ts.isVariableDeclarationList !== 'function' || typeof ts.forEachChild !== 'function') {
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
  if (typeof checker.getAwaitedType !== 'function' || typeof checker.getPropertyOfType !== 'function') {
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
    const primitive = ts.TypeFlags.StringLike | ts.TypeFlags.NumberLike | ts.TypeFlags.BooleanLike |
      ts.TypeFlags.BigIntLike | ts.TypeFlags.Void | ts.TypeFlags.Null | ts.TypeFlags.Undefined |
      ts.TypeFlags.ESSymbolLike;
    return !!(type.flags & primitive) && !checker.getPropertyOfType(type, 'then');
  }
  function directlyAwaited(node) {
    let value = node;
    while (value.parent && ts.isParenthesizedExpression(value.parent) && value.parent.expression === value) value = value.parent;
    return !!value.parent && ts.isAwaitExpression(value.parent) && value.parent.expression === value;
  }
  // Only a direct function-body return without using declarations is safe:
  // finally blocks, iterator cleanup and resource disposal can replace it.
  function returnedFromAsync(node) {
    let value = node;
    while (value.parent && ts.isParenthesizedExpression(value.parent) && value.parent.expression === value) value = value.parent;
    let current = value.parent;
    if (current && ts.isArrowFunction(current) && current.body === value) return asyncFunction(current);
    if (!current || !ts.isReturnStatement(current) || current.expression !== value) return false;
    const body = current.parent;
    const fn = body && body.parent;
    return !!fn && ts.isBlock(body) && ts.isFunctionLike(fn) && fn.body === body && asyncFunction(fn) && !hasUsing(body);
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
    // prefix for every call. Equal starts retain traversal order, so the
    // innermost call wins just as it did in the original lookup.
    nodes.sort((a, b) => a.start - b.start || a.order - b.order);
    const index = new Map();
    let previous = 0;
    let bytes = 0;
    for (const entry of nodes) {
      bytes += Buffer.byteLength(file.text.slice(previous, entry.start), 'utf8');
      index.set(bytes, entry.node);
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
    if (!selected) return { ...call, status: 'unavailable' };
    const type = checker.getTypeAtLocation(selected);
    return { ...call, status: promiseLike(type) ? 'promise_like' : definitelyNotPromise(type) ? 'not_promise' : directlyAwaited(selected) || returnedFromAsync(selected) ? 'observed' : 'not_proven' };
  });
  return { status: 'ok', calls };
}

try {
  const request = JSON.parse(fs.readFileSync(0, 'utf8'));
  process.stdout.write(JSON.stringify(run(request)));
} catch (_) {
  process.stdout.write(JSON.stringify({ status: 'incomplete', reason: 'checker_unavailable' }));
}
