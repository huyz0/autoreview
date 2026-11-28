// Resolve the runtime module graph from compiler-emitted CommonJS without
// executing project code. Any dynamic or unavailable local edge fails closed.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

function stable(value) {
  if (Array.isArray(value)) return value.map(stable);
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map(key => [key, stable(value[key])]));
  return value;
}

function run(request) {
  const ts = require(request.typescriptModule);
  const config = ts.readConfigFile(request.config, ts.sys.readFile);
  if (config.error) return { status: 'incomplete' };
  const parsed = ts.parseJsonConfigFileContent(config.config, ts.sys, path.dirname(request.config), undefined, request.config);
  if (parsed.errors.length || parsed.options.module !== ts.ModuleKind.CommonJS || parsed.options.noEmit ||
      parsed.options.emitDeclarationOnly || parsed.options.outFile) return { status: 'incomplete' };
  const program = ts.createProgram(parsed.fileNames, { ...parsed.options, noEmit: false });
  if (ts.getPreEmitDiagnostics(program).some(d => d.category === ts.DiagnosticCategory.Error)) return { status: 'incomplete' };
  const optionsSha256 = crypto.createHash('sha256').update(JSON.stringify(stable(parsed.options))).digest('hex');
  const projectSources = program.getSourceFiles().filter(file => !file.isDeclarationFile && !program.isSourceFileFromExternalLibrary(file)).map(file => ({
    path: file.fileName, sha256: crypto.createHash('sha256').update(file.text, 'utf8').digest('hex'),
  }));
  if (!projectSources.length || projectSources.length > 512) return { status: 'incomplete' };
  if (projectSources.reduce((total, item) => total + fs.statSync(item.path).size, 0) > 64 * 1024 * 1024) return { status: 'incomplete' };
  const root = fs.realpathSync(request.root);
  const indexes = new Map();
  let totalBytes = 0;
  for (let i = 0; i < request.files.length; i++) {
    const item = request.files[i], actual = fs.realpathSync(item.path);
    if (!actual.startsWith(root + path.sep) || indexes.has(actual)) return { status: 'incomplete' };
    const size = fs.statSync(actual).size;
    totalBytes += size;
    if (size > 4 * 1024 * 1024 || totalBytes > 64 * 1024 * 1024) return { status: 'incomplete' };
    const file = program.getSourceFile(item.path);
    if (!file || crypto.createHash('sha256').update(file.text, 'utf8').digest('hex') !== item.sha256) return { status: 'incomplete' };
    indexes.set(actual, i);
  }
  const reachable = new Set(), pending = [...request.entries];
  while (pending.length) {
    const index = pending.pop();
    if (!Number.isInteger(index) || index < 0 || index >= request.files.length) return { status: 'incomplete' };
    if (reachable.has(index)) continue;
    reachable.add(index);
    const file = program.getSourceFile(request.files[index].path);
    let output = null, count = 0;
    const result = program.emit(file, (name, text) => {
      if (/\.(?:c|m)?js$/i.test(name)) { output = text; count++; }
    });
    if (result.emitSkipped || count !== 1 || output.length > 4 * 1024 * 1024) return { status: 'incomplete' };
    const emitted = ts.createSourceFile('emitted.js', output, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
    let valid = true, remaining = 200000;
    function visit(node) {
      if (!valid || --remaining < 0) { valid = false; return; }
      if (ts.isIdentifier(node) && ['eval', 'Function', 'global', 'globalThis', 'module', 'createRequire'].includes(node.text)) { valid = false; return; }
      if (ts.isIdentifier(node) && node.text === 'require') {
        const call = node.parent;
        if (!ts.isCallExpression(call) || call.expression !== node || call.arguments.length !== 1 || !ts.isStringLiteral(call.arguments[0])) { valid = false; return; }
        const specifier = call.arguments[0].text;
        const resolution = ts.resolveModuleName(specifier, file.fileName, parsed.options, ts.sys).resolvedModule;
        if (specifier.startsWith('.') && !resolution) { valid = false; return; }
        if (resolution) {
          let target;
          try { target = fs.realpathSync(resolution.resolvedFileName); } catch (_) { valid = false; return; }
          if (target.startsWith(root + path.sep) && !target.includes(`${path.sep}node_modules${path.sep}`)) {
            const next = indexes.get(target);
            if (next === undefined) { valid = false; return; }
            pending.push(next);
          }
        }
      }
      ts.forEachChild(node, visit);
    }
    visit(emitted);
    if (!valid) return { status: 'incomplete' };
  }
  // Inputs must still match the source read by the compiler after emitting.
  for (const item of [...request.files, ...projectSources]) {
    if (fs.statSync(item.path).size > 4 * 1024 * 1024) return { status: 'incomplete' };
    if (crypto.createHash('sha256').update(fs.readFileSync(item.path)).digest('hex') !== item.sha256) return { status: 'incomplete' };
  }
  const current = ts.readConfigFile(request.config, ts.sys.readFile);
  if (current.error) return { status: 'incomplete' };
  const again = ts.parseJsonConfigFileContent(current.config, ts.sys, path.dirname(request.config), undefined, request.config);
  if (again.errors.length || crypto.createHash('sha256').update(JSON.stringify(stable(again.options))).digest('hex') !== optionsSha256) return { status: 'incomplete' };
  return { status: 'ok', reachable: [...reachable].sort((a, b) => a - b), projectSources, optionsSha256 };
}

try { process.stdout.write(JSON.stringify(run(JSON.parse(fs.readFileSync(0, 'utf8'))))); }
catch (_) { process.stdout.write(JSON.stringify({ status: 'incomplete' })); }
