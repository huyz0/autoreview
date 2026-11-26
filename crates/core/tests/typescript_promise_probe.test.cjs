const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { createHash } = require('node:crypto');

const compiler = process.env.AUTOREVIEW_TEST_TYPESCRIPT;
const probe = path.resolve(__dirname, '../src/analyzers/typescript_promise_probe.cjs');

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
    const target = path.resolve(directory);
    const root = path.resolve(os.tmpdir()) + path.sep;
    if (!target.startsWith(root) || !path.basename(target).startsWith('autoreview-ts-probe-')) throw new Error('Unexpected temporary directory');
    fs.rmSync(target, { recursive: true, force: true });
  }
});

test('checks concrete call types and fails closed on project errors', { skip: !compiler }, () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'autoreview-ts-probe-'));
  try {
    const config = path.join(directory, 'tsconfig.json');
    const file = path.join(directory, 'sample.ts');
    const types = path.join(directory, 'node_modules', '@types', 'local');
    fs.mkdirSync(types, { recursive: true });
    fs.writeFileSync(path.join(types, 'index.d.ts'), 'declare const LOCAL_GLOBAL: number;');
    fs.writeFileSync(config, JSON.stringify({ compilerOptions: { target: 'ES2022', strict: true }, files: ['sample.ts'] }));
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
    const target = path.resolve(directory);
    const root = path.resolve(os.tmpdir()) + path.sep;
    if (!target.startsWith(root) || !path.basename(target).startsWith('autoreview-ts-probe-')) throw new Error('Unexpected temporary directory');
    fs.rmSync(target, { recursive: true, force: true });
  }
});
