// Prototype: read one JSON request from stdin and write one JSON response.
// The caller must provide the TypeScript package path explicitly.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

function stable(value) {
  if (Array.isArray(value)) return value.map(stable);
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map(key => [key, stable(value[key])]));
  return value;
}
function optionsHash(options) {
  return crypto.createHash('sha256').update(JSON.stringify(stable(options))).digest('hex');
}

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
  // Optional implementation evidence for the audited @actions/core 1.1.3
  // package. The caller explicitly attests that the runtime module is not
  // replaced or mutated; a void declaration alone is never sufficient.
  const actionsContracts = {
    '@actions/core': { declaration: 'a22018715e553dc26173579a0a1e45db6809ec5c987f1b9a44765cff56c04095',
      implementation: 'bcdf9ccb9dfa4f9d06ab5923f3bd48ec2b317494c8f6a520d634388062d58041', methods: ['info', 'debug', 'warning', 'setSecret'] },
    '@actions/core/lib/command': { declaration: 'ec547f2ac4dffe1c61446c2b409eaeffbb0a18ba340ccf5a7a1387655aa76bfb',
      implementation: '7f5dbcd3ef92c5b29eb2c59b64f808ca16a90e3a650b448811d76ab946de87ac', methods: ['issueCommand'] },
  };
  const actionsCache = new Map();
  const actionsProjectCache = new Map();
  const proofFiles = new Map();
  function fileHash(file, limit = 1024 * 1024) {
    const stat = fs.statSync(file);
    if (!stat.isFile() || stat.size > limit) return null;
    return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
  }
  function proofFile(file, expected, limit) {
    if (fileHash(file, limit) !== expected) return false;
    proofFiles.set(file, { expected, limit });
    return true;
  }
  function namespaceImport(node) {
    const symbol = checker.getSymbolAtLocation(node);
    if (!symbol || symbol.declarations?.length !== 1 || !ts.isNamespaceImport(symbol.declarations[0])) return null;
    const decl = symbol.declarations[0].parent?.parent;
    return decl && ts.isImportDeclaration(decl) && ts.isStringLiteral(decl.moduleSpecifier) ? { symbol, name: decl.moduleSpecifier.text } : null;
  }
  function namespaceOnlyCalls(file, binding, nested = false) {
    let valid = true, remaining = 500000;
    function visit(node) {
      if (!valid || --remaining < 0) { valid = false; return; }
      if (ts.isIdentifier(node) && checker.getSymbolAtLocation(node) === binding && !ts.isNamespaceImport(node.parent)) {
        if (nested && isTypePosition(node)) { ts.forEachChild(node, visit); return; }
        let access = node.parent;
        if (!ts.isPropertyAccessExpression(access) || access.expression !== node) { valid = false; return; }
        if (nested && access.name.text === 'promises') {
          while (ts.isPropertyAccessExpression(access.parent) && access.parent.expression === access) access = access.parent;
        }
        if (!ts.isCallExpression(access.parent) || access.parent.expression !== access || access.parent.questionDotToken) valid = false;
      }
      ts.forEachChild(node, visit);
    }
    visit(file);
    return valid;
  }
  function actionsProjectUnmodified(moduleName, implementation) {
    const cacheKey = `${moduleName}\0${implementation}`;
    if (actionsProjectCache.has(cacheKey)) return actionsProjectCache.get(cacheKey);
    let valid = true, remaining = 500000;
    for (const file of program.getSourceFiles()) {
      if (file.isDeclarationFile || program.isSourceFileFromExternalLibrary(file)) continue;
      function visit(node) {
        if (!valid || --remaining < 0) { valid = false; return; }
        if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
          let resolved;
          try { resolved = fs.realpathSync(require.resolve(node.moduleSpecifier.text, { paths: [path.dirname(file.fileName)] })); } catch (_) {}
          if (resolved === implementation || node.moduleSpecifier.text === moduleName || node.moduleSpecifier.text === `node:${moduleName}`) {
            if (!ts.isImportDeclaration(node)) { valid = false; return; }
            const namespace = node.importClause?.namedBindings;
            if (!namespace || !ts.isNamespaceImport(namespace) || node.importClause.name) { valid = false; return; }
            const binding = checker.getSymbolAtLocation(namespace.name);
            if (!binding || !namespaceOnlyCalls(file, binding, moduleName === 'fs')) { valid = false; return; }
          }
        }
        if ((ts.isCallExpression(node) || ts.isNewExpression(node)) && ts.isIdentifier(node.expression) &&
            ['eval', 'Function'].includes(node.expression.text)) { valid = false; return; }
        if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword) { valid = false; return; }
        if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'require') {
          if (node.arguments.length !== 1 || !ts.isStringLiteral(node.arguments[0])) { valid = false; return; }
          const name = node.arguments[0].text;
          let resolved;
          try { resolved = fs.realpathSync(require.resolve(name, { paths: [path.dirname(file.fileName)] })); } catch (_) {}
          if (name === moduleName || name === `node:${moduleName}` || /^@actions\/core(?:\/|$)/.test(name) || resolved === implementation) { valid = false; return; }
        }
        ts.forEachChild(node, visit);
      }
      visit(file);
      if (!valid) break;
    }
    actionsProjectCache.set(cacheKey, valid);
    return valid;
  }
  function auditedActionsCall(call) {
    if (process.env.AUTOREVIEW_TYPESCRIPT_TRUST_ACTIONS_CORE_1_1_3 !== '1' || parsed.options.module !== ts.ModuleKind.CommonJS ||
        !ts.isPropertyAccessExpression(call.expression) || !ts.isIdentifier(call.expression.expression) || call.questionDotToken) return false;
    const binding = namespaceImport(call.expression.expression);
    const contract = binding && actionsContracts[binding.name];
    if (!contract || !contract.methods.includes(call.expression.name.text)) return false;
    const declaration = checker.getResolvedSignature(call)?.declaration?.getSourceFile();
    if (!declaration || crypto.createHash('sha256').update(declaration.text, 'utf8').digest('hex') !== contract.declaration) return false;
    const implementation = path.join(path.dirname(declaration.fileName), binding.name.endsWith('/command') ? 'command.js' : 'core.js');
    let actual;
    try {
      actual = fs.realpathSync(require.resolve(binding.name, { paths: [path.dirname(call.getSourceFile().fileName)] }));
      if (actual !== fs.realpathSync(implementation)) return false;
    } catch (_) { return false; }
    const key = `${call.getSourceFile().fileName}\0${binding.name}\0${actual}`;
    if (!actionsCache.has(key)) {
      let valid = false;
      try { valid = proofFile(declaration.fileName, contract.declaration) && proofFile(actual, contract.implementation) && proofProjectSources() && actionsProjectUnmodified(binding.name, actual); } catch (_) {}
      actionsCache.set(key, valid);
    }
    return actionsCache.get(key);
  }
  const node12Binaries = new Set([
    'b014e4ec5ca810b2fb54cdbf6ab8d6acc488285c98469606efb8b412472bec2a', // official Windows x64
    '5a3c51dbee8fdd7201fe7b531f03262754b9b9fb1008f4c824e8d649c4e9c96b', // official Linux x64
  ]);
  const node12Globals = '4d0e3b8334392a2e0adef9caf867118678fb8f38da90d8003184debd62d30f3b';
  const node12ExtraGlobals = '4926e99d2ad39c0bbd36f2d37cc8f52756bc7a5661ad7b12815df871a4b07ba1';
  const node12Assert = '58a52f282f1dad18179023804c1f3a7536bf875748a0a314abd43f60fd5b244f';
  const node12Fs = '6bd120d96313412155ae7d4766d586a38576a058b3e269be2152db7030fbc882';
  const node12Url = '1e3da92862604b1f7a32265169f9aa712c4567742d42597704e04ae3e07019e7';
  let node12Runtime;
  function trustedNode12() {
    if (node12Runtime !== undefined) return node12Runtime;
    node12Runtime = false;
    const binary = process.env.AUTOREVIEW_TYPESCRIPT_TRUST_NODE_12_22_12;
    if (!binary || !path.isAbsolute(binary)) return false;
    try {
      const actual = fileHash(binary, 64 * 1024 * 1024);
      if (node12Binaries.has(actual) && proofFile(binary, actual, 64 * 1024 * 1024)) node12Runtime = true;
    } catch (_) {}
    return node12Runtime;
  }
  function declarationHash(declaration, expected) {
    const file = declaration?.getSourceFile();
    if (!file || crypto.createHash('sha256').update(file.text, 'utf8').digest('hex') !== expected) return false;
    try { return proofFile(file.fileName, expected); } catch (_) { return false; }
  }
  function isTypePosition(node) {
    for (let current = node.parent, depth = 0; current && depth < 16; current = current.parent, depth++) {
      if (ts.isTypeNode(current)) return true;
      if (ts.isExpression(current) || ts.isStatement(current) || ts.isDeclaration(current)) return false;
    }
    return false;
  }
  const nodeGlobalCache = new Map();
  function globalBindingUnmodified(name, symbol) {
    if (nodeGlobalCache.has(symbol)) return nodeGlobalCache.get(symbol);
    let valid = true, remaining = 500000;
    for (const file of program.getSourceFiles()) {
      if (file.isDeclarationFile || program.isSourceFileFromExternalLibrary(file)) continue;
      function visit(node) {
        if (!valid || --remaining < 0) { valid = false; return; }
        if (ts.isIdentifier(node) && ['global', 'globalThis'].includes(node.text) && !isTypePosition(node)) { valid = false; return; }
        if ((ts.isCallExpression(node) || ts.isNewExpression(node)) && ts.isIdentifier(node.expression) &&
            ['eval', 'Function', 'require'].includes(node.expression.text)) { valid = false; return; }
        if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword) { valid = false; return; }
        if (ts.isIdentifier(node) && node.text === name && checker.getSymbolAtLocation(node) === symbol && !isTypePosition(node)) {
          const parent = node.parent;
          if (name === 'setTimeout') {
            if (!ts.isCallExpression(parent) || parent.expression !== node || parent.questionDotToken) valid = false;
          } else if (['String', 'RegExp', 'Object', 'Array', 'Uint8Array', 'Promise'].includes(name) && (ts.isCallExpression(parent) || ts.isNewExpression(parent)) && parent.expression === node) {
            if (parent.questionDotToken) valid = false;
          } else if (!ts.isPropertyAccessExpression(parent) || parent.expression !== node ||
                     !ts.isCallExpression(parent.parent) || parent.parent.expression !== parent || parent.parent.questionDotToken) valid = false;
        }
        ts.forEachChild(node, visit);
      }
      visit(file);
      if (!valid) break;
    }
    nodeGlobalCache.set(symbol, valid);
    return valid;
  }
  function trustedStandardDeclaration(declaration) {
    const file = declaration?.getSourceFile();
    if (!file || !file.isDeclarationFile || !/^lib\..*\.d\.ts$/.test(path.basename(file.fileName))) return false;
    try {
      const directory = fs.realpathSync(path.dirname(ts.getDefaultLibFilePath(parsed.options)));
      const actual = fs.realpathSync(file.fileName);
      if (path.dirname(actual) !== directory) return false;
      const hash = crypto.createHash('sha256').update(file.text, 'utf8').digest('hex');
      return proofFile(actual, hash);
    } catch (_) { return false; }
  }
  function primitiveString(type) {
    return type.isUnion() ? type.types.every(primitiveString) : !!(type.flags & ts.TypeFlags.StringLike);
  }
  function trustedStandardSymbolDeclaration(declaration) {
    return trustedStandardDeclaration(declaration) || declarationHash(declaration, node12Globals);
  }
  function safeBuiltinPrototypes(location) {
    return ['Object', 'Array', 'Uint8Array', 'Promise'].every(name => {
      const symbol = checker.resolveName(name, location, ts.SymbolFlags.Value, false);
      return !!symbol && symbol.declarations?.length > 0 && symbol.declarations.every(trustedStandardSymbolDeclaration) && globalBindingUnmodified(name, symbol);
    });
  }
  function auditedStandardCall(call, declaration) {
    const expr = call.expression;
    if (!ts.isPropertyAccessExpression(expr) || !trustedStandardDeclaration(declaration)) return false;
    if (ts.isIdentifier(expr.expression) && expr.expression.text === 'Object' && expr.name.text === 'keys') {
      const symbol = checker.getSymbolAtLocation(expr.expression);
      return !!symbol && symbol.declarations?.length > 0 && symbol.declarations.every(trustedStandardSymbolDeclaration) && globalBindingUnmodified('Object', symbol);
    }
    const supported = expr.name.text === 'match' && call.arguments.length === 1 && ts.isRegularExpressionLiteral(call.arguments[0]) ||
      expr.name.text === 'split' && call.arguments.length === 1 && ts.isStringLiteral(call.arguments[0]);
    if (!supported || !primitiveString(checker.getTypeAtLocation(expr.expression))) return false;
    const symbol = checker.resolveName('String', expr, ts.SymbolFlags.Value, false);
    if (!symbol || !symbol.declarations?.length || !symbol.declarations.every(trustedStandardSymbolDeclaration) || !globalBindingUnmodified('String', symbol)) return false;
    if (expr.name.text === 'match') {
      const regexp = checker.resolveName('RegExp', expr, ts.SymbolFlags.Value, false);
      return !!regexp && regexp.declarations?.length > 0 && regexp.declarations.every(trustedStandardSymbolDeclaration) && globalBindingUnmodified('RegExp', regexp);
    }
    return true;
  }
  function auditedNode12Call(call) {
    if (!trustedNode12() || !proofProjectSources() || !thenMutationAbsent() || call.questionDotToken) return false;
    if (!safeBuiltinPrototypes(call)) return false;
    const expr = call.expression;
    const declaration = checker.getResolvedSignature(call)?.declaration;
    if (ts.isPropertyAccessExpression(expr) && ts.isIdentifier(expr.expression)) {
      const receiver = expr.expression;
      if (receiver.text === 'Buffer' && expr.name.text === 'from') {
        const symbol = checker.getSymbolAtLocation(receiver);
        return !!symbol && declarationHash(declaration, node12Globals) && symbol.declarations?.length > 0 &&
          symbol.declarations.every(item => declarationHash(item, node12Globals) || declarationHash(item, node12ExtraGlobals)) &&
          globalBindingUnmodified('Buffer', symbol);
      }
      const binding = namespaceImport(receiver);
      if (!binding) return auditedStandardCall(call, declaration);
      if (binding.name === 'assert' && expr.name.text === 'ok') return declarationHash(declaration, node12Assert) && actionsProjectUnmodified('assert', 'builtin:assert');
      if (binding.name === 'fs' && expr.name.text === 'statSync') return declarationHash(declaration, node12Fs) && actionsProjectUnmodified('fs', 'builtin:fs');
      return auditedStandardCall(call, declaration);
    }
    if (ts.isIdentifier(expr) && expr.text === 'setTimeout') {
      const symbol = checker.getSymbolAtLocation(expr);
      return !!symbol && declarationHash(declaration, node12Globals) && symbol.declarations?.length > 0 &&
        symbol.declarations.every(item => declarationHash(item, node12Globals)) && globalBindingUnmodified('setTimeout', symbol);
    }
    return auditedStandardCall(call, declaration);
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
  const projectFunctionCache = new Map();
  function typeOnlyNamedImport(clause) {
    const named = clause?.namedBindings;
    if (clause?.name || !named || !ts.isNamedImports(named)) return false;
    return named.elements.every(item => {
      const symbol = checker.getSymbolAtLocation(item.name);
      if (!symbol || !(symbol.flags & ts.SymbolFlags.Alias)) return false;
      const target = checker.getAliasedSymbol(symbol);
      return !!target && !(target.flags & ts.SymbolFlags.Value);
    });
  }
  function namespaceUsesMethodSafely(file, binding, method) {
    let valid = true, remaining = 500000;
    function visit(node) {
      if (!valid || --remaining < 0) { valid = false; return; }
      if (ts.isIdentifier(node) && checker.getSymbolAtLocation(node) === binding && !ts.isNamespaceImport(node.parent)) {
        if (isTypePosition(node)) { ts.forEachChild(node, visit); return; }
        const access = node.parent;
        if (!ts.isPropertyAccessExpression(access) || access.expression !== node) { valid = false; return; }
        const called = ts.isCallExpression(access.parent) && access.parent.expression === access && !access.parent.questionDotToken;
        const interpolated = ts.isTemplateSpan(access.parent) && access.parent.expression === access;
        if (!called && (access.name.text === method || !interpolated && !definitelyNotPromise(checker.getTypeAtLocation(access)))) valid = false;
      }
      ts.forEachChild(node, visit);
    }
    visit(file);
    return valid;
  }
  function projectFunctionUnmodified(moduleSymbol, functionSymbol, method, declaration) {
    if (projectFunctionCache.has(declaration)) return projectFunctionCache.get(declaration);
    let valid = true, remaining = 500000;
    for (const file of program.getSourceFiles()) {
      if (file.isDeclarationFile || program.isSourceFileFromExternalLibrary(file)) continue;
      function visit(node) {
        if (!valid || --remaining < 0) { valid = false; return; }
        if (ts.isIdentifier(node) && ['eval', 'Function', 'require', 'module', 'exports'].includes(node.text)) { valid = false; return; }
        if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword) { valid = false; return; }
        if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier &&
            checker.getSymbolAtLocation(node.moduleSpecifier) === moduleSymbol) {
          if (!ts.isImportDeclaration(node)) { valid = false; return; }
          const namespace = node.importClause?.namedBindings;
          if (typeOnlyNamedImport(node.importClause)) { ts.forEachChild(node, visit); return; }
          if (!namespace || !ts.isNamespaceImport(namespace) || node.importClause.name) { valid = false; return; }
          const binding = checker.getSymbolAtLocation(namespace.name);
          if (!binding || !namespaceUsesMethodSafely(file, binding, method)) { valid = false; return; }
        }
        if (ts.isIdentifier(node) && checker.getSymbolAtLocation(node) === functionSymbol && node !== declaration.name) {
          const parent = node.parent;
          const direct = ts.isCallExpression(parent) && parent.expression === node && !parent.questionDotToken;
          const member = ts.isPropertyAccessExpression(parent) && !parent.questionDotToken && parent.name === node && ts.isCallExpression(parent.parent) && parent.parent.expression === parent && !parent.parent.questionDotToken;
          if (!direct && !member) { valid = false; return; }
        }
        ts.forEachChild(node, visit);
      }
      visit(file);
      if (!valid) break;
    }
    projectFunctionCache.set(declaration, valid);
    return valid;
  }
  let projectSourcesVerified;
  function proofProjectSources() {
    if (projectSourcesVerified !== undefined) return projectSourcesVerified;
    let total = 0, count = 0;
    try {
      for (const file of program.getSourceFiles()) {
        if (file.isDeclarationFile || program.isSourceFileFromExternalLibrary(file)) continue;
        const bytes = Buffer.byteLength(file.text, 'utf8');
        if (++count > 512 || bytes > 4 * 1024 * 1024 || (total += bytes) > 64 * 1024 * 1024) return projectSourcesVerified = false;
        const hash = crypto.createHash('sha256').update(file.text, 'utf8').digest('hex');
        if (!proofFile(file.fileName, hash, 4 * 1024 * 1024)) return projectSourcesVerified = false;
      }
      return projectSourcesVerified = count > 0;
    } catch (_) { return projectSourcesVerified = false; }
  }
  function auditedProjectFunction(call) {
    if (!graphVerified || parsed.options.module !== ts.ModuleKind.CommonJS || call.questionDotToken) return false;
    const expression = call.expression;
    const binding = ts.isPropertyAccessExpression(expression) && ts.isIdentifier(expression.expression) ? namespaceImport(expression.expression) : null;
    if (!binding && !ts.isIdentifier(expression) || binding && !binding.name.startsWith('.')) return false;
    const declaration = checker.getResolvedSignature(call)?.declaration;
    if (!declaration || !ts.isFunctionDeclaration(declaration) || !declaration.name || !declaration.body ||
        program.isSourceFileFromExternalLibrary(declaration.getSourceFile()) ||
        !(localReturnsNoValue(declaration) || auditedUrlFactory(declaration))) return false;
    const actualModule = checker.getSymbolAtLocation(declaration.getSourceFile());
    const namespace = binding?.symbol.declarations[0];
    const importDecl = namespace?.parent?.parent;
    const moduleSymbol = binding ? importDecl && checker.getSymbolAtLocation(importDecl.moduleSpecifier) : actualModule;
    const functionSymbol = checker.getSymbolAtLocation(declaration.name);
    const calledName = binding ? expression.name : expression;
    if (!moduleSymbol || moduleSymbol !== actualModule || !functionSymbol || checker.getSymbolAtLocation(calledName) !== functionSymbol || !proofProjectSources() ||
        !binding && declaration.getSourceFile() !== call.getSourceFile()) return false;
    const file = declaration.getSourceFile(), hash = crypto.createHash('sha256').update(file.text, 'utf8').digest('hex');
    try { return proofFile(file.fileName, hash) && projectFunctionUnmodified(moduleSymbol, functionSymbol, calledName.text, declaration); } catch (_) { return false; }
  }
  function auditedUrlFactory(declaration) {
    if (!trustedNode12() || asyncFunction(declaration) || declaration.asteriskToken || declaration.body.statements.length !== 1) return false;
    const statement = declaration.body.statements[0], construct = statement.expression;
    if (!ts.isReturnStatement(statement) || !construct || !ts.isNewExpression(construct) || !ts.isIdentifier(construct.expression) ||
        !declarationHash(checker.getResolvedSignature(construct)?.declaration, node12Url) || !safeBuiltinPrototypes(construct)) return false;
    const symbol = checker.getSymbolAtLocation(construct.expression);
    if (!symbol || symbol.declarations?.length !== 1 || !ts.isImportSpecifier(symbol.declarations[0])) return false;
    const specifier = symbol.declarations[0], importDecl = specifier.parent?.parent?.parent;
    if (!importDecl || !ts.isImportDeclaration(importDecl) || !ts.isStringLiteral(importDecl.moduleSpecifier) || importDecl.moduleSpecifier.text !== 'url' ||
        (specifier.propertyName?.text || specifier.name.text) !== 'URL') return false;
    return urlImportsUnmodified();
  }
  let urlImportsSafe;
  function urlImportsUnmodified() {
    if (urlImportsSafe !== undefined) return urlImportsSafe;
    let valid = true, remaining = 500000;
    for (const file of program.getSourceFiles()) {
      if (file.isDeclarationFile || program.isSourceFileFromExternalLibrary(file)) continue;
      function visit(node) {
        if (!valid || --remaining < 0) { valid = false; return; }
        if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier) && ['url', 'node:url'].includes(node.moduleSpecifier.text)) {
          const clause = node.importClause, named = clause?.namedBindings;
          if (clause?.name || !named || !ts.isNamedImports(named)) { valid = false; return; }
          for (const item of named.elements) {
            if ((item.propertyName?.text || item.name.text) !== 'URL') { valid = false; return; }
            const symbol = checker.getSymbolAtLocation(item.name);
            if (!symbol || !urlConstructorOnly(file, symbol, item.name)) { valid = false; return; }
          }
        }
        if (ts.isExportDeclaration(node) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier) && ['url', 'node:url'].includes(node.moduleSpecifier.text)) { valid = false; return; }
        ts.forEachChild(node, visit);
      }
      visit(file);
      if (!valid) break;
    }
    urlImportsSafe = valid;
    return valid;
  }
  function urlConstructorOnly(file, symbol, declaration) {
    let valid = true, remaining = 500000;
    function visit(node) {
      if (!valid || --remaining < 0) { valid = false; return; }
      if (ts.isIdentifier(node) && node !== declaration && checker.getSymbolAtLocation(node) === symbol && !isTypePosition(node) &&
          (!ts.isNewExpression(node.parent) || node.parent.expression !== node)) { valid = false; return; }
      ts.forEachChild(node, visit);
    }
    visit(file);
    return valid;
  }
  // Closed-program receiver proof for private readonly fields. This follows
  // only uniquely assigned variables, explicit returns, and uniquely resolved
  // local functions/static methods. Anything escaping that subset is unknown.
  let projectNodes;
  function allProjectNodes() {
    if (projectNodes !== undefined) return projectNodes;
    const nodes = [], pending = program.getSourceFiles().filter(file => !file.isDeclarationFile && !program.isSourceFileFromExternalLibrary(file));
    while (pending.length) {
      const node = pending.pop();
      if (nodes.length >= 500000) return projectNodes = null;
      nodes.push(node);
      ts.forEachChild(node, child => { pending.push(child); });
    }
    return projectNodes = nodes;
  }
  function assignmentTo(node) {
    const parent = node.parent;
    return ts.isBinaryExpression(parent) && parent.left === node && parent.operatorToken.kind >= ts.SyntaxKind.FirstAssignment && parent.operatorToken.kind <= ts.SyntaxKind.LastAssignment ||
      (ts.isPrefixUnaryExpression(parent) || ts.isPostfixUnaryExpression(parent)) && [ts.SyntaxKind.PlusPlusToken, ts.SyntaxKind.MinusMinusToken].includes(parent.operator) ||
      ts.isDeleteExpression(parent);
  }
  function directCallsOnly(name) {
    const nodes = allProjectNodes();
    if (!nodes) return false;
    return nodes.every(node => {
      if (ts.isPropertyAccessExpression(node) && node.name.text === name) return ts.isCallExpression(node.parent) && node.parent.expression === node && !node.parent.questionDotToken;
      if (ts.isElementAccessExpression(node) && node.argumentExpression && ts.isStringLiteral(node.argumentExpression) && node.argumentExpression.text === name) return false;
      return true;
    });
  }
  function thenMutationAbsent() {
    if (!directCallsOnly('then')) return false;
    const nodes = allProjectNodes();
    if (!nodes) return false;
    return nodes.every(node => {
      if (ts.isElementAccessExpression(node) && assignmentTo(node) && (!node.argumentExpression || !ts.isStringLiteral(node.argumentExpression))) {
        if (freshObject(node.expression)) return true;
        const parent = node.parent;
        if (ts.isDeleteExpression(parent)) return true;
        return ts.isBinaryExpression(parent) && parent.operatorToken.kind === ts.SyntaxKind.EqualsToken && definitelyNotPromise(checker.getTypeAtLocation(parent.right));
      }
      if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && ts.isIdentifier(node.expression.expression) &&
          ['Object', 'Reflect'].includes(node.expression.expression.text)) {
        const method = node.expression.name.text;
        if (['assign', 'defineProperties', 'setPrototypeOf'].includes(method)) return false;
        if (['defineProperty', 'set'].includes(method)) {
          const key = node.arguments[1];
          return !!key && ts.isStringLiteral(key) && key.text !== 'then';
        }
      }
      return true;
    });
  }
  function symbolWrites(symbol, declaration) {
    const nodes = allProjectNodes();
    if (!nodes) return null;
    const values = declaration.initializer ? [declaration.initializer] : [];
    for (const node of nodes) {
      if (!ts.isIdentifier(node) || node === declaration.name || checker.getSymbolAtLocation(node) !== symbol) continue;
      if (!assignmentTo(node)) continue;
      if (!ts.isBinaryExpression(node.parent) || node.parent.operatorToken.kind !== ts.SyntaxKind.EqualsToken) return null;
      values.push(node.parent.right);
    }
    return values.length === 1 ? values[0] : null;
  }
  function parameterUnmodified(declaration) {
    if (declaration.initializer || declaration.dotDotDotToken || !ts.isIdentifier(declaration.name)) return false;
    const nodes = allProjectNodes(), symbol = checker.getSymbolAtLocation(declaration.name);
    return !!nodes && !!symbol && !nodes.some(item => ts.isIdentifier(item) && item !== declaration.name && checker.getSymbolAtLocation(item) === symbol && assignmentTo(item));
  }
  function functionCalls(declaration) {
    const nodes = allProjectNodes();
    if (!nodes || !declaration.name || !ts.isIdentifier(declaration.name)) return null;
    const symbol = checker.getSymbolAtLocation(declaration.name), moduleSymbol = checker.getSymbolAtLocation(declaration.getSourceFile());
    if (!symbol || !moduleSymbol || !projectFunctionUnmodified(moduleSymbol, symbol, declaration.name.text, declaration)) return null;
    const calls = nodes.filter(node => ts.isCallExpression(node) && checker.getResolvedSignature(node)?.declaration === declaration);
    return calls.length > 0 && calls.length <= 128 ? calls : null;
  }
  function classIsClosed(declaration) {
    if (!ts.isClassDeclaration(declaration) || !declaration.name || declaration.heritageClauses?.length ||
        declaration.modifiers?.some(item => [ts.SyntaxKind.ExportKeyword, ts.SyntaxKind.AbstractKeyword, ts.SyntaxKind.DeclareKeyword, ts.SyntaxKind.Decorator].includes(item.kind)) ||
        declaration.members.some(member => member.kind === ts.SyntaxKind.ClassStaticBlockDeclaration || member.name && ts.isComputedPropertyName(member.name) ||
          member.modifiers?.some(item => item.kind === ts.SyntaxKind.Decorator))) return false;
    const nodes = allProjectNodes(), symbol = checker.getSymbolAtLocation(declaration.name);
    if (!nodes || !symbol || symbol.declarations?.length !== 1) return false;
    return nodes.every(node => {
      if (!ts.isIdentifier(node) || node === declaration.name || checker.getSymbolAtLocation(node) !== symbol || isTypePosition(node)) return true;
      const parent = node.parent;
      if (ts.isNewExpression(parent) && parent.expression === node) return true;
      return ts.isPropertyAccessExpression(parent) && parent.expression === node && ts.isCallExpression(parent.parent) && parent.parent.expression === parent;
    });
  }
  function returnedExpressions(declaration) {
    if (!declaration.body || !ts.isBlock(declaration.body)) return null;
    const pending = [declaration.body], values = [];
    let remaining = 10000;
    while (pending.length) {
      if (--remaining < 0) return null;
      const node = pending.pop();
      if (node !== declaration.body && ts.isFunctionLike(node)) continue;
      if (ts.isReturnStatement(node)) values.push(node.expression || null);
      ts.forEachChild(node, child => { pending.push(child); });
    }
    return values.length && values.length <= 128 ? values : null;
  }
  function nonNullable(node) {
    const type = checker.getTypeAtLocation(node);
    return !(type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.Null | ts.TypeFlags.Undefined)) &&
      (!type.isUnion() || type.types.every(item => !(item.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.Null | ts.TypeFlags.Undefined))));
  }
  function classValue(node, awaited, seen, depth) {
    if (!node || depth > 32 || seen.has(node)) return null;
    seen.add(node);
    if (ts.isParenthesizedExpression(node)) return classValue(node.expression, awaited, seen, depth + 1);
    if (ts.isAwaitExpression(node)) return classValue(node.expression, true, seen, depth + 1);
    if (node.kind === ts.SyntaxKind.NullKeyword) return { classes: new Set(), nullable: true };
    if (ts.isIdentifier(node) && node.text === 'undefined') {
      const intrinsic = checker.getSymbolAtLocation(node);
      if (intrinsic && !intrinsic.declarations?.length && checker.getTypeAtLocation(node).flags & ts.TypeFlags.Undefined) return { classes: new Set(), nullable: true };
    }
    if (ts.isNewExpression(node) && ts.isIdentifier(node.expression)) {
      const symbol = checker.getSymbolAtLocation(node.expression), declaration = symbol?.declarations?.[0];
      if (!symbol || symbol.declarations.length !== 1 || !classIsClosed(declaration)) return null;
      const constructor = declaration.members.find(ts.isConstructorDeclaration);
      if (constructor && !localReturnsNoValue(constructor)) return null;
      if (declaration.members.some(member => member.name && ts.isIdentifier(member.name) && member.name.text === 'then')) return null;
      return { classes: new Set([declaration]), nullable: false };
    }
    if (ts.isIdentifier(node)) {
      const symbol = checker.getSymbolAtLocation(node), declaration = symbol?.declarations?.[0];
      if (!symbol || symbol.declarations.length !== 1) return null;
      if (ts.isVariableDeclaration(declaration)) {
        const value = symbolWrites(symbol, declaration);
        return value && classValue(value, awaited, seen, depth + 1);
      }
      if (ts.isParameter(declaration) && ts.isFunctionDeclaration(declaration.parent)) {
        const fn = declaration.parent, index = fn.parameters.indexOf(declaration), calls = functionCalls(fn);
        if (index < 0 || !calls || !parameterUnmodified(declaration)) return null;
        return mergeClassValues(calls.map(call => call.arguments[index] && narrowedClassValue(call.arguments[index], seen, depth + 1)));
      }
      return null;
    }
    if (ts.isCallExpression(node)) {
      if (node.questionDotToken) return null;
      const declaration = checker.getResolvedSignature(node)?.declaration;
      if (!declaration || asyncFunction(declaration) && !awaited || declaration.asteriskToken) return null;
      if (ts.isFunctionDeclaration(declaration)) {
        if (!functionCalls(declaration)) return null;
      } else if (ts.isMethodDeclaration(declaration)) {
        const owner = declaration.parent, expr = node.expression;
        if (!ts.isClassDeclaration(owner) || !classIsClosed(owner) || !declaration.modifiers?.some(item => item.kind === ts.SyntaxKind.StaticKeyword) ||
            !ts.isPropertyAccessExpression(expr) || !ts.isIdentifier(expr.expression) || checker.getSymbolAtLocation(expr.expression) !== checker.getSymbolAtLocation(owner.name) ||
            !directCallsOnly(expr.name.text)) return null;
      } else return null;
      const returns = returnedExpressions(declaration);
      if (!returns) return null;
      const result = mergeClassValues(returns.map(value => value ? classValue(value, false, new Set(seen), depth + 1) : { classes: new Set(), nullable: true }));
      if (!result) return null;
      // A function may also fall through. The caller's narrowed type must
      // prove non-null before a nullable result can be used as a receiver.
      result.nullable = true;
      return result;
    }
    return null;
  }
  function mergeClassValues(values) {
    if (values.some(value => !value)) return null;
    const classes = new Set(), nullable = values.some(value => value.nullable);
    for (const value of values) for (const declaration of value.classes) classes.add(declaration);
    return { classes, nullable };
  }
  function narrowedClassValue(node, seen, depth) {
    const result = classValue(node, false, new Set(seen), depth);
    if (!result || result.nullable && !nonNullable(node)) return null;
    return { classes: result.classes, nullable: false };
  }
  function fieldAssignment(owner, field) {
    const constructors = owner.members.filter(ts.isConstructorDeclaration);
    if (constructors.length !== 1 || !constructors[0].body) return null;
    const ctor = constructors[0], fieldSymbol = checker.getSymbolAtLocation(field.name), matches = [];
    for (const statement of ctor.body.statements) {
      if (!ts.isExpressionStatement(statement) || !ts.isBinaryExpression(statement.expression)) continue;
      const assignment = statement.expression, left = assignment.left;
      if (assignment.operatorToken.kind === ts.SyntaxKind.EqualsToken && ts.isPropertyAccessExpression(left) && left.expression.kind === ts.SyntaxKind.ThisKeyword &&
          checker.getSymbolAtLocation(left.name) === fieldSymbol && ts.isIdentifier(assignment.right)) matches.push(assignment);
    }
    if (matches.length !== 1) return null;
    const parameterSymbol = checker.getSymbolAtLocation(matches[0].right);
    const index = ctor.parameters.findIndex(param => ts.isIdentifier(param.name) && checker.getSymbolAtLocation(param.name) === parameterSymbol);
    return index < 0 || !parameterUnmodified(ctor.parameters[index]) ? null : { assignment: matches[0], index };
  }
  function fieldUnmodified(field, assignment) {
    const nodes = allProjectNodes();
    if (!nodes || !ts.isIdentifier(field.name)) return false;
    return nodes.every(node => {
      if (ts.isPropertyAccessExpression(node) && node.name.text === field.name.text && assignmentTo(node)) return node.parent === assignment;
      if (ts.isElementAccessExpression(node) && assignmentTo(node)) {
        const arg = node.argumentExpression;
        if (!arg || !ts.isStringLiteral(arg)) {
          if (freshObject(node.expression)) return true;
          const type = checker.getTypeAtLocation(node.expression);
          if (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) return false;
          const owner = checker.getTypeAtLocation(field.parent);
          return !checker.isTypeAssignableTo(type, owner) && !checker.isTypeAssignableTo(owner, type);
        }
        return arg.text !== field.name.text;
      }
      return true;
    });
  }
  function freshObject(node) {
    if (!ts.isIdentifier(node)) return false;
    const symbol = checker.getSymbolAtLocation(node), declaration = symbol?.declarations?.[0];
    return !!symbol && symbol.declarations.length === 1 && ts.isVariableDeclaration(declaration) &&
      ts.isVariableDeclarationList(declaration.parent) && !!(declaration.parent.flags & ts.NodeFlags.Const) &&
      !!declaration.initializer && ts.isObjectLiteralExpression(declaration.initializer) && declaration.initializer.properties.length === 0 &&
      symbolWrites(symbol, declaration) === declaration.initializer;
  }
  function targetMutationSafe(owner, concrete, fieldName, methodName) {
    const nodes = allProjectNodes(), types = [owner, concrete].map(item => checker.getDeclaredTypeOfSymbol(checker.getSymbolAtLocation(item.name)));
    if (!nodes || types.some(type => !type)) return false;
    function possibleTarget(expression) {
      if (freshObject(expression)) return false;
      const type = checker.getTypeAtLocation(expression);
      return !!(type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) || types.some(target => checker.isTypeAssignableTo(type, target) || checker.isTypeAssignableTo(target, type));
    }
    return nodes.every(node => {
      if (ts.isElementAccessExpression(node) && assignmentTo(node)) {
        const key = node.argumentExpression;
        if (key && ts.isStringLiteral(key)) return ![fieldName, methodName, 'then', '__proto__'].includes(key.text);
        return !possibleTarget(node.expression);
      }
      if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && ts.isIdentifier(node.expression.expression) &&
          ['Object', 'Reflect'].includes(node.expression.expression.text) && ['assign', 'defineProperty', 'defineProperties', 'setPrototypeOf', 'set'].includes(node.expression.name.text)) {
        return !node.arguments.length || !possibleTarget(node.arguments[0]);
      }
      return true;
    });
  }
  function noVisibleRebinding() {
    const nodes = allProjectNodes();
    if (!nodes) return false;
    return nodes.every(node => {
      if (ts.isPropertyAccessExpression(node) && ['call', 'apply', 'bind'].includes(node.name.text)) return false;
      if (ts.isElementAccessExpression(node) && node.argumentExpression && ts.isStringLiteral(node.argumentExpression) && ['call', 'apply', 'bind'].includes(node.argumentExpression.text)) return false;
      return !ts.isIdentifier(node) || node.text !== 'Proxy' || isTypePosition(node);
    });
  }
  function concreteReceiver(call) {
    if (!graphVerified || !trustedNode12() || !safeBuiltinPrototypes(call) || !thenMutationAbsent() ||
        !(parsed.options.strictNullChecks ?? parsed.options.strict) || !(parsed.options.alwaysStrict ?? parsed.options.strict) || !noVisibleRebinding() ||
        !proofProjectSources() || call.questionDotToken || !ts.isPropertyAccessExpression(call.expression)) return false;
    const access = call.expression, fieldAccess = access.expression;
    if (!ts.isPropertyAccessExpression(fieldAccess) || fieldAccess.expression.kind !== ts.SyntaxKind.ThisKeyword || !directCallsOnly(access.name.text)) return false;
    let enclosing = call.parent;
    while (enclosing && !ts.isFunctionLike(enclosing)) enclosing = enclosing.parent;
    if (!enclosing || !ts.isMethodDeclaration(enclosing) || !enclosing.name || !ts.isIdentifier(enclosing.name) ||
        enclosing.modifiers?.some(item => item.kind === ts.SyntaxKind.StaticKeyword) || !directCallsOnly(enclosing.name.text)) return false;
    const fieldSymbol = checker.getSymbolAtLocation(fieldAccess.name), field = fieldSymbol?.declarations?.[0];
    if (!fieldSymbol || fieldSymbol.declarations.length !== 1 || !ts.isPropertyDeclaration(field) || !ts.isIdentifier(field.name) || field.initializer ||
        !field.modifiers?.some(item => item.kind === ts.SyntaxKind.PrivateKeyword) || !field.modifiers?.some(item => item.kind === ts.SyntaxKind.ReadonlyKeyword)) return false;
    const owner = field.parent;
    if (!classIsClosed(owner)) return false;
    const assignment = fieldAssignment(owner, field);
    if (!assignment || !fieldUnmodified(field, assignment.assignment)) return false;
    const nodes = allProjectNodes(), ownerSymbol = checker.getSymbolAtLocation(owner.name);
    if (!nodes || !ownerSymbol) return false;
    const creations = nodes.filter(node => ts.isNewExpression(node) && ts.isIdentifier(node.expression) && checker.getSymbolAtLocation(node.expression) === ownerSymbol);
    if (!creations.length || creations.length > 128) return false;
    const values = mergeClassValues(creations.map(node => node.arguments?.[assignment.index] && narrowedClassValue(node.arguments[assignment.index], new Set(), 0)));
    if (!values || values.nullable || values.classes.size !== 1) return false;
    const [concrete] = values.classes;
    if (!targetMutationSafe(owner, concrete, field.name.text, access.name.text)) return false;
    const methods = concrete.members.filter(member => ts.isMethodDeclaration(member) && member.name && ts.isIdentifier(member.name) && member.name.text === access.name.text &&
      !member.modifiers?.some(item => item.kind === ts.SyntaxKind.StaticKeyword));
    if (methods.length !== 1 || !localReturnsNoValue(methods[0])) return false;
    // The compiler-observed sources that support this proof must remain stable.
    try {
      for (const declaration of [owner, concrete]) {
        const file = declaration.getSourceFile(), hash = crypto.createHash('sha256').update(file.text, 'utf8').digest('hex');
        if (!proofFile(file.fileName, hash)) return false;
      }
      return true;
    } catch (_) { return false; }
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
  let graphVerified = false;
  if (request.graphEvidence !== undefined) {
    const evidence = request.graphEvidence;
    const project = program.getSourceFiles().filter(file => !file.isDeclarationFile && !program.isSourceFileFromExternalLibrary(file));
    if (process.env.AUTOREVIEW_TYPESCRIPT_TRUST_EMITTED_GRAPH !== '1' || !evidence || !Array.isArray(evidence.sources) ||
        !evidence.sources.length || evidence.sources.length > 512 || evidence.sources.length !== project.length ||
        evidence.optionsSha256 !== optionsHash(parsed.options)) return { status: 'incomplete', reason: 'checker_unavailable' };
    const expected = new Map(evidence.sources.map(source => [fs.realpathSync(source.path), source.sha256]));
    if (expected.size !== project.length) return { status: 'incomplete', reason: 'checker_unavailable' };
    for (const file of project) {
      const hash = expected.get(fs.realpathSync(file.fileName));
      if (!hash || crypto.createHash('sha256').update(file.text, 'utf8').digest('hex') !== hash ||
          !proofFile(file.fileName, hash, 4 * 1024 * 1024)) return { status: 'incomplete', reason: 'checker_unavailable' };
    }
    graphVerified = true;
  }
  const files = new Map();
  function indexCalls(file) {
    const nodes = [];
    function visit(node) {
      if (ts.isCallExpression(node)) nodes.push({ node, start: node.getStart(file), end: node.getEnd(), order: nodes.length });
      ts.forEachChild(node, visit);
    }
    visit(file);
    // Accumulate UTF-8 offsets in source order instead of encoding the whole
    // prefix for every call. Keep all calls with the same starting offset:
    // a request without an end offset cannot distinguish them.
    nodes.sort((a, b) => a.start - b.start || a.order - b.order);
    const offsets = new Map();
    let previous = 0;
    let bytes = 0;
    for (const offset of [...new Set(nodes.flatMap(entry => [entry.start, entry.end]))].sort((a, b) => a - b)) {
      bytes += Buffer.byteLength(file.text.slice(previous, offset), 'utf8');
      offsets.set(offset, bytes);
      previous = offset;
    }
    const index = new Map();
    for (const entry of nodes) {
      const start = offsets.get(entry.start);
      if (!index.has(start)) index.set(start, []);
      index.get(start).push({ node: entry.node, end: offsets.get(entry.end) });
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
    let selected = cached.calls.get(call.start);
    if (!selected) return { ...call, status: 'unavailable' };
    if (call.end !== undefined) {
      selected = selected.filter(entry => entry.end === call.end);
      if (selected.length !== 1) return { ...call, status: 'unavailable' };
    }
    const statuses = selected.map(({ node }) => {
      const type = checker.getTypeAtLocation(node);
      const local = localFunctionLiteral(node);
      return promiseLike(type) || local && asyncFunction(local) ? 'promise_like' : definitelyNotPromise(type) || localReturnsNoValue(local) || auditedActionsCall(node) || auditedNode12Call(node) || auditedProjectFunction(node) || concreteReceiver(node) ? 'not_promise' : directlyAwaited(node) || immediatelyAwaitedBinding(node) || returnedFromAsync(node) ? 'observed' : 'not_proven';
    });
    const status = statuses.length === 1 ? statuses[0] :
      statuses.every(item => item === 'not_promise' || item === 'observed') ? 'observed' : 'not_proven';
    return { ...call, status };
  });
  for (const [file, proof] of proofFiles) {
    if (fileHash(file, proof.limit) !== proof.expected) return { status: 'incomplete', reason: 'checker_unavailable' };
  }
  if (graphVerified) {
    const current = ts.readConfigFile(request.config, ts.sys.readFile);
    if (current.error) return { status: 'incomplete', reason: 'checker_unavailable' };
    const again = ts.parseJsonConfigFileContent(current.config, ts.sys, path.dirname(request.config), undefined, request.config);
    if (again.errors.length || optionsHash(again.options) !== request.graphEvidence.optionsSha256) return { status: 'incomplete', reason: 'checker_unavailable' };
  }
  return { status: 'ok', calls };
}

try {
  const request = JSON.parse(fs.readFileSync(0, 'utf8'));
  process.stdout.write(JSON.stringify(run(request)));
} catch (_) {
  process.stdout.write(JSON.stringify({ status: 'incomplete', reason: 'checker_unavailable' }));
}
