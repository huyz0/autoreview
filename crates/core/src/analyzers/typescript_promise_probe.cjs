// Prototype: read one JSON request from stdin and write one JSON response.
// The caller must provide the TypeScript package path explicitly.
const fs = require('node:fs');
const path = require('node:path');

function run(request) {
  const ts = require(request.typescriptModule);
  const config = ts.readConfigFile(request.config, ts.sys.readFile);
  if (config.error) return { status: 'incomplete', reason: 'config_unreadable' };
  const parsed = ts.parseJsonConfigFileContent(config.config, ts.sys, path.dirname(request.config));
  if (parsed.errors.length) return { status: 'incomplete', reason: 'config_invalid' };
  const program = ts.createProgram(parsed.fileNames, { ...parsed.options, noEmit: true });
  if (ts.getPreEmitDiagnostics(program).some(diagnostic => diagnostic.category === ts.DiagnosticCategory.Error)) {
    return { status: 'incomplete', reason: 'project_type_errors' };
  }
  const checker = program.getTypeChecker();
  function promiseLike(type) {
    if (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.Never)) return false;
    if (type.isUnion()) return type.types.every(promiseLike);
    if (!(type.flags & ts.TypeFlags.Object) || !checker.getPropertyOfType(type, 'then')) return false;
    const awaited = checker.getAwaitedType(type);
    return !!awaited && awaited !== type;
  }
  const calls = request.calls.map(call => {
    const file = program.getSourceFile(call.path);
    if (!file) return { ...call, status: 'unavailable' };
    let selected;
    function visit(node) {
      if (ts.isCallExpression(node) && Buffer.byteLength(file.text.slice(0, node.getStart(file)), 'utf8') === call.start) selected = node;
      ts.forEachChild(node, visit);
    }
    visit(file);
    if (!selected) return { ...call, status: 'unavailable' };
    return { ...call, status: promiseLike(checker.getTypeAtLocation(selected)) ? 'promise_like' : 'not_proven' };
  });
  return { status: 'ok', calls };
}

try {
  const request = JSON.parse(fs.readFileSync(0, 'utf8'));
  process.stdout.write(JSON.stringify(run(request)));
} catch (_) {
  process.stdout.write(JSON.stringify({ status: 'incomplete', reason: 'checker_unavailable' }));
}
