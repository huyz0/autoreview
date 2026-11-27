const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { createHash } = require('node:crypto');

const compiler = process.env.AUTOREVIEW_TEST_TYPESCRIPT;
const probe = path.resolve(__dirname, '../src/analyzers/typescript_promise_probe.cjs');

function removeTemporaryDirectory(directory) {
  const target = path.resolve(directory);
  const root = path.resolve(os.tmpdir()) + path.sep;
  if (!target.startsWith(root) || !path.basename(target).startsWith('autoreview-ts-probe-')) throw new Error('Unexpected temporary directory');
  fs.rmSync(target, { recursive: true, force: true });
}

function probeFixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'autoreview-ts-probe-'));
  t.after(() => removeTemporaryDirectory(directory));
  const file = path.join(directory, 'sample.ts');
  const config = path.join(directory, 'tsconfig.json');
  fs.writeFileSync(config, JSON.stringify({ compilerOptions: { target: 'ES2020', strict: true }, files: ['sample.ts'] }));
  return { file, config };
}

test('distinguishes an unsupported compiler API from a missing module', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'autoreview-ts-probe-'));
  try {
    const native = path.join(directory, 'typescript');
    fs.mkdirSync(native);
    fs.writeFileSync(path.join(native, 'package.json'), JSON.stringify({ name: 'typescript', version: '7.0.2', type: 'module' }));
    const oldApi = path.join(directory, 'unsupported.cjs');
    fs.writeFileSync(oldApi, 'module.exports = {};');
    const invoke = typescriptModule => {
      const child = spawnSync(process.execPath, [probe], { input: JSON.stringify({ typescriptModule, config: 'unused', calls: [] }), encoding: 'utf8' });
      assert.equal(child.status, 0, child.stderr);
      return JSON.parse(child.stdout);
    };
    assert.deepEqual(invoke(native), { status: 'incomplete', reason: 'compiler_api_unsupported' });
    assert.deepEqual(invoke(oldApi), { status: 'incomplete', reason: 'compiler_api_unsupported' });
    assert.deepEqual(invoke(path.join(directory, 'missing')), { status: 'incomplete', reason: 'checker_unavailable' });
  } finally {
    removeTemporaryDirectory(directory);
  }
});

test('proves return values only for immutable local synchronous function literals', { skip: !compiler }, t => {
  const { file, config } = probeFixture(t);
  const source = [
    'const sync: () => void = () => { if (Math.random()) return; const nested = async () => 1; };',
    'const explicit: () => void = () => { try { return; } finally { return void 1; } };',
    'const expression: () => void = () => void 1;',
    'const asynchronous: () => void = async () => {};',
    'const asyncExpression: () => void = async function () {};',
    'const generator: () => void = async function* () { yield 1; };',
    'const hidden: () => void = () => Promise.resolve();',
    'const guarded: () => void = () => { try { return; } finally { return Promise.resolve(); } };',
    'let mutable: () => void = () => {}; mutable = async () => {};',
    `const large: () => void = () => { ${'void 0;'.repeat(10001)} };`,
    'function shadow(sync: () => void) { sync(); }',
    'sync(); explicit(); expression(); asynchronous(); asyncExpression(); generator(); hidden(); guarded(); mutable(); large();',
  ].join('\n');
  fs.writeFileSync(file, source);
  const names = ['sync()', 'explicit()', 'expression()', 'asynchronous()', 'asyncExpression()', 'generator()', 'hidden()', 'guarded()', 'mutable()', 'large()'];
  const digest = createHash('sha256').update(source, 'utf8').digest('hex');
  const calls = names.map(name => ({ path: file, start: Buffer.byteLength(source.slice(0, source.lastIndexOf(name))), source_sha256: digest }));
  calls.push({ path: file, start: Buffer.byteLength(source.slice(0, source.indexOf('sync();'))), source_sha256: digest });
  const child = spawnSync(process.execPath, [probe], { input: JSON.stringify({ typescriptModule: compiler, config, calls }), encoding: 'utf8' });
  assert.equal(child.status, 0, child.stderr);
  const result = JSON.parse(child.stdout);
  assert.equal(result.status, 'ok');
  assert.deepEqual(result.calls.map(call => call.status), ['not_promise', 'not_promise', 'not_promise', 'promise_like', 'promise_like', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'not_proven']);
});

test('classifies shared offsets only when every call is safe', { skip: !compiler }, t => {
  const { file, config } = probeFixture(t);
  const source = [
    'declare function outerPromise(): string & (() => Promise<void>);',
    'declare function innerPromise(): Promise<void> & (() => number);',
    'declare function bothSynchronous(): string;',
    'declare function unknownOuter(): string & (() => any);',
    'declare function ordinary(): number;',
    "const prefix = '😀'; outerPromise()(); innerPromise()(); bothSynchronous().trim(); unknownOuter()(); ordinary();",
  ].join('\n');
  fs.writeFileSync(file, source);
  const digest = createHash('sha256').update(source, 'utf8').digest('hex');
  const calls = ['outerPromise()()', 'innerPromise()()', 'bothSynchronous().trim()', 'unknownOuter()()', 'ordinary()'].map(name => ({
    path: file, start: Buffer.byteLength(source.slice(0, source.lastIndexOf(name))), source_sha256: digest,
  }));
  const child = spawnSync(process.execPath, [probe], { input: JSON.stringify({ typescriptModule: compiler, config, calls }), encoding: 'utf8' });
  assert.equal(child.status, 0, child.stderr);
  const result = JSON.parse(child.stdout);
  assert.equal(result.status, 'ok');
  assert.deepEqual(result.calls.map(call => call.status), ['not_proven', 'not_proven', 'observed', 'not_proven', 'not_promise']);
  const start = Buffer.byteLength(source.slice(0, source.lastIndexOf('outerPromise()()')));
  const explicit = ['outerPromise()', 'outerPromise()()', 'outerPromise()()x'].map(text => ({
    path: file, start, end: start + Buffer.byteLength(text), source_sha256: digest,
  }));
  const exactChild = spawnSync(process.execPath, [probe], { input: JSON.stringify({ typescriptModule: compiler, config, calls: explicit }), encoding: 'utf8' });
  assert.equal(exactChild.status, 0, exactChild.stderr);
  assert.deepEqual(JSON.parse(exactChild.stdout).calls.map(call => call.status), ['not_proven', 'promise_like', 'unavailable']);
});

test('checks concrete call types and fails closed on project errors', { skip: !compiler }, () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'autoreview-ts-probe-'));
  try {
    const config = path.join(directory, 'tsconfig.json');
    const file = path.join(directory, 'sample.ts');
    const types = path.join(directory, 'node_modules', '@types', 'local');
    fs.mkdirSync(types, { recursive: true });
    fs.writeFileSync(path.join(types, 'index.d.ts'), 'declare const LOCAL_GLOBAL: number;');
    fs.writeFileSync(config, JSON.stringify({ compilerOptions: { target: 'ES2020', strict: true }, files: ['sample.ts'] }));
    const source = [
      'declare function promise(): Promise<void>;',
      'declare function sync(): string;',
      'declare function anything(): any;',
      'declare function mixed(): Promise<void> | string;',
      'declare function thenable(): PromiseLike<number>;',
      'declare function factory(): () => Promise<void>;',
      "const accent = 'é';",
      "const face = '😀';",
      'const localGlobal = LOCAL_GLOBAL;',
      'promise(); sync(); anything(); mixed(); thenable(); factory()();',
      'function generic<T>(value: T) { return identity(value); }',
      'declare function identity<T>(value: T): T;',
      'sync();'.repeat(250),
    ].join('\n');
    fs.writeFileSync(file, source);
    const names = ['promise()', 'sync()', 'anything()', 'mixed()', 'thenable()', 'identity(value)', 'factory()()'];
    const hash = text => createHash('sha256').update(text, 'utf8').digest('hex');
    let calls = names.map(name => ({ path: file, start: Buffer.byteLength(source.slice(0, source.lastIndexOf(name))), source_sha256: hash(source) }));
    function invoke(typescriptModule = compiler, env = {}) {
      const child = spawnSync(process.execPath, [probe], { input: JSON.stringify({ typescriptModule, config, calls }), encoding: 'utf8', env: { ...process.env, ...env } });
      assert.equal(child.status, 0, child.stderr);
      return JSON.parse(child.stdout);
    }
    const checked = invoke();
    assert.equal(checked.status, 'ok');
    assert.deepEqual(checked.calls.map(call => call.status), ['promise_like', 'not_promise', 'not_proven', 'not_proven', 'promise_like', 'not_proven', 'not_proven']);
    const awaiting = 'declare function unknownValue(): any; declare function knownPromise(): Promise<void>; async function run() { await ((unknownValue())); await unknownValue().value; await knownPromise(); unknownValue(); }';
    fs.writeFileSync(file, awaiting);
    calls = [...awaiting.matchAll(/\b(?:unknownValue|knownPromise)\(\)/g)].slice(2)
      .map(match => ({ path: file, start: Buffer.byteLength(awaiting.slice(0, match.index)), source_sha256: hash(awaiting) }));
    assert.deepEqual(invoke().calls.map(call => call.status), ['observed', 'not_proven', 'promise_like', 'not_proven']);
    const bindings = [
      'declare function unknownValue(): any;',
      'async function direct() { const value = unknownValue(); await value; }',
      'async function wrapped() { const value = ((unknownValue())); await ((value)); }',
      'async function aliases() { const value = unknownValue(); const alias = value; const other = (alias); await other; }',
      'async function later() { const value = unknownValue(); log(); await value; }',
      'async function conditional(flag: boolean) { const value = unknownValue(); if (flag) await value; }',
      'async function reassigned() { let value = unknownValue(); value = 1; await value; }',
      'async function multi() { const value = unknownValue(), other = 1; await value; }',
      'async function property() { const value = unknownValue(); await value.item; }',
      'async function different() { const value = unknownValue(); const other = 1; await other; }',
      'async function embedded() { const value = [unknownValue()]; await value; }',
      'async function mutableAlias() { const value = unknownValue(); let alias = value; await alias; }',
      'async function unrelatedAlias() { const value = unknownValue(); const alias = 1; await alias; }',
      `async function atLimit() { const v0 = unknownValue(); ${Array.from({ length: 31 }, (_, i) => `const v${i + 1} = v${i};`).join(' ')} await v31; }`,
      `async function overLimit() { const v0 = unknownValue(); ${Array.from({ length: 32 }, (_, i) => `const v${i + 1} = v${i};`).join(' ')} await v32; }`,
      'declare function log(): void;',
    ].join('\n');
    fs.writeFileSync(file, bindings);
    calls = [...bindings.matchAll(/\bunknownValue\(\)/g)].slice(1)
      .map(match => ({ path: file, start: Buffer.byteLength(bindings.slice(0, match.index)), source_sha256: hash(bindings) }));
    assert.deepEqual(invoke().calls.map(call => call.status), ['observed', 'observed', 'observed', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'observed', 'not_proven']);
    const returns = [
      'declare function unknownValue(): any;',
      'async function direct() { return ((unknownValue())); }',
      'const arrow = async () => (unknownValue());',
      'class Example { async method() { return unknownValue(); } }',
      'async function branch(flag: boolean) { if (flag) { return unknownValue(); } else return unknownValue(); }',
      'async function choose(value: number) { switch (value) { case 1: return unknownValue(); default: return unknownValue(); } }',
      'async function guarded() { try { return unknownValue(); } finally { return 1; } }',
      'async function nestedGuard(flag: boolean) { try { if (flag) return unknownValue(); } finally { return 1; } }',
      'async function loop() { for (const value of [1]) { return unknownValue(); } return 1; }',
      'function syncReturn() { return unknownValue(); }',
      'async function outer() { function inner() { return unknownValue(); } return inner(); }',
      'async function* generator() { return unknownValue(); }',
      'async function property() { return unknownValue().value; }',
    ].join('\n');
    fs.writeFileSync(file, returns);
    calls = [...returns.matchAll(/\bunknownValue\(\)/g)].slice(1)
      .map(match => ({ path: file, start: Buffer.byteLength(returns.slice(0, match.index)), source_sha256: hash(returns) }));
    assert.deepEqual(invoke().calls.map(call => call.status), ['observed', 'observed', 'observed', 'observed', 'observed', 'observed', 'observed', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'not_proven']);
    const expressions = [
      'declare function unknownValue(): any;',
      'async function branch(flag: boolean) { return flag ? unknownValue() : unknownValue(); }',
      'async function awaited(flag: boolean) { await (flag ? unknownValue() : unknownValue()); }',
      'async function logical(flag: boolean) { return flag && unknownValue() || unknownValue(); }',
      'async function nullish(flag: boolean) { return flag ?? unknownValue(); }',
      'async function condition() { return unknownValue() ? 1 : 2; }',
      'async function left() { return unknownValue() || 1; }',
      'async function guarded(flag: boolean) { try { return flag ? unknownValue() : 1; } finally { return 1; } }',
      'function sync(flag: boolean) { return flag ? unknownValue() : 1; }',
      'async function property(flag: boolean) { return (flag ? unknownValue() : 1).value; }',
    ].join('\n');
    fs.writeFileSync(file, expressions);
    calls = [...expressions.matchAll(/\bunknownValue\(\)/g)].slice(1)
      .map(match => ({ path: file, start: Buffer.byteLength(expressions.slice(0, match.index)), source_sha256: hash(expressions) }));
    assert.deepEqual(invoke().calls.map(call => call.status), ['observed', 'observed', 'observed', 'observed', 'not_proven', 'observed', 'observed', 'not_proven', 'not_proven', 'not_proven', 'not_proven', 'not_proven']);
    if (require(compiler).NodeFlags.Using) {
      const resources = [
        'declare function unknownValue(): any;',
        'declare const resource: { [Symbol.dispose](): void };',
        'async function work() { using item = resource; return unknownValue(); }',
        'async function outer() { function inner() { using item = resource; } return unknownValue(); }',
        'async function awaitUsingSource() { await using value = unknownValue(); await value; }',
        'async function awaitUsingAlias() { const value = unknownValue(); await using alias = value; await alias; }',
      ].join('\n');
      fs.writeFileSync(config, JSON.stringify({ compilerOptions: { target: 'ES2020', lib: ['ES2020', 'ESNext.Disposable'], strict: true }, files: ['sample.ts'] }));
      fs.writeFileSync(file, resources);
      calls = [...resources.matchAll(/\bunknownValue\(\)/g)].slice(1)
        .map(match => ({ path: file, start: Buffer.byteLength(resources.slice(0, match.index)), source_sha256: hash(resources) }));
      assert.deepEqual(invoke().calls.map(call => call.status), ['not_proven', 'observed', 'not_proven', 'not_proven']);
      fs.writeFileSync(config, JSON.stringify({ compilerOptions: { target: 'ES2020', strict: true }, files: ['sample.ts'] }));
    }
    fs.writeFileSync(file, source);
    calls = names.map(name => ({ path: file, start: Buffer.byteLength(source.slice(0, source.lastIndexOf(name))), source_sha256: hash(source) }));
    calls = [calls[1], calls[0], calls[1], { ...calls[0], source_sha256: 'wrong' }, { ...calls[0], path: path.join(directory, 'missing.ts') }];
    assert.deepEqual(invoke().calls.map(call => call.status), ['not_promise', 'promise_like', 'not_promise', 'source_changed', 'unavailable']);
    calls = Array.from(source.matchAll(/\bsync\(\);/g), match => ({ path: file, start: Buffer.byteLength(source.slice(0, match.index)), source_sha256: hash(source) })).reverse();
    const bulk = invoke().calls;
    assert.equal(bulk.length, 251);
    assert.deepEqual(bulk.map(call => call.start), calls.map(call => call.start));
    assert.ok(bulk.every(call => call.status === 'not_promise'));
    const shadowed = 'export {}; type Promise<T> = string; declare function fake(): Promise<void>; fake();';
    fs.writeFileSync(file, shadowed);
    calls = [{ path: file, start: Buffer.byteLength(shadowed.slice(0, shadowed.lastIndexOf('fake()'))), source_sha256: hash(shadowed) }];
    assert.equal(invoke().calls[0].status, 'not_promise');
    calls[0].source_sha256 = hash(source);
    assert.equal(invoke().calls[0].status, 'source_changed');
    fs.appendFileSync(file, '\nconst broken: number = "wrong";\n');
    assert.deepEqual(invoke(), { status: 'incomplete', reason: 'project_type_errors' });

    const compatibility = 'declare function task(): Promise<void>; try { throw new Error(); } catch (error) { error.message; } task();';
    fs.writeFileSync(file, compatibility);
    calls = [{ path: file, start: Buffer.byteLength(compatibility.slice(0, compatibility.lastIndexOf('task()'))), source_sha256: hash(compatibility) }];
    assert.deepEqual(invoke(), { status: 'incomplete', reason: 'project_type_errors' });
    const adapter = path.resolve(__dirname, '../../../benchmarks/typescript-legacy-catch.cjs');
    const env = { AUTOREVIEW_BENCHMARK_TYPESCRIPT_MODULE: compiler };
    assert.equal(invoke(adapter, env).calls[0].status, 'promise_like');
    fs.appendFileSync(file, '\nconst broken: number = "wrong";\n');
    assert.deepEqual(invoke(adapter, env), { status: 'incomplete', reason: 'project_type_errors' });
  } finally {
    removeTemporaryDirectory(directory);
  }
});
