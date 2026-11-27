const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { createHash } = require('node:crypto');

const compiler = process.env.AUTOREVIEW_TEST_TYPESCRIPT;
const actionsCore = process.env.AUTOREVIEW_TEST_ACTIONS_CORE;
const nodeTypes = process.env.AUTOREVIEW_TEST_NODE12_TYPES;
const node12 = process.env.AUTOREVIEW_TEST_NODE12_BINARY;
const probe = path.resolve(__dirname, '../src/analyzers/typescript_promise_probe.cjs');
const runtimeGraph = path.resolve(__dirname, '../src/analyzers/typescript_runtime_graph.cjs');

function graphEvidenceFor(root, config, paths) {
  const files = paths.map(p => ({ path: p, sha256: createHash('sha256').update(fs.readFileSync(p)).digest('hex') }));
  const child = spawnSync(process.execPath, [runtimeGraph], { input: JSON.stringify({ typescriptModule: compiler, config, root, files, entries: [0] }), encoding: 'utf8' });
  assert.equal(child.status, 0, child.stderr);
  const result = JSON.parse(child.stdout);
  return result.status === 'ok' ? { sources: result.projectSources, optionsSha256: result.optionsSha256 } : undefined;
}

test('runtime graph excludes erased imports and fails closed for dynamic or missing edges', { skip: !compiler }, t => {
  const { file, config } = probeFixture(t), root = path.dirname(file);
  fs.writeFileSync(config, JSON.stringify({ compilerOptions: { target: 'ES2020', module: 'CommonJS', strict: true }, files: ['sample.ts', 'types.ts', 'live.ts'] }));
  const types = path.join(root, 'types.ts'), live = path.join(root, 'live.ts');
  fs.writeFileSync(types, 'export interface I { x: number }\n');
  fs.writeFileSync(live, 'export function f() {}\n');
  function invoke(source, omitted = false) {
    fs.writeFileSync(file, source);
    const files = (omitted ? [file, types] : [file, types, live]).map(p => ({ path: p, sha256: createHash('sha256').update(fs.readFileSync(p)).digest('hex') }));
    const child = spawnSync(process.execPath, [runtimeGraph], { input: JSON.stringify({ typescriptModule: compiler, config, root, files, entries: [0] }), encoding: 'utf8' });
    assert.equal(child.status, 0, child.stderr);
    return JSON.parse(child.stdout);
  }
  assert.deepEqual(invoke("import { I } from './types'; import { f } from './live'; const x: I = { x: 1 }; f();").reachable, [0, 2]);
  assert.deepEqual(invoke("import './types'; import { f } from './live'; f();").reachable, [0, 1, 2]);
  assert.equal(invoke("import { f } from './live'; f();", true).status, 'incomplete');
  assert.equal(invoke("declare function require(p: string): unknown; declare const name: string; require(name);").status, 'incomplete');
  assert.equal(invoke("eval('1 + 1');").status, 'incomplete');
});

test('project function evidence requires a closed, unmodified module and audited URL constructor', { skip: !compiler || !nodeTypes || !node12 }, t => {
  const { file, config } = probeFixture(t), root = path.dirname(file), library = path.join(root, 'library.ts');
  const typeDir = path.join(root, 'node_modules/@types/node');
  fs.mkdirSync(path.dirname(typeDir), { recursive: true });
  fs.cpSync(nodeTypes, typeDir, { recursive: true });
  fs.writeFileSync(config, JSON.stringify({ compilerOptions: { target: 'ES2020', lib: ['ES2020'], module: 'CommonJS', strict: true, skipLibCheck: true }, files: ['sample.ts', 'library.ts'] }));
  const librarySource = "import {URL} from 'url'; export function store(): void {} export function makeUrl(): URL { return new URL('https://example.com'); }";
  fs.writeFileSync(library, librarySource);
  function invoke(source, needle, trust, stale = false) {
    fs.writeFileSync(file, source);
    const start = Buffer.byteLength(source.slice(0, source.lastIndexOf(needle)));
    const call = { path: file, start, end: start + Buffer.byteLength(needle), source_sha256: createHash('sha256').update(source).digest('hex') };
    const graphEvidence = trust ? graphEvidenceFor(root, config, [file, library]) : undefined;
    if (stale === 'options') graphEvidence.optionsSha256 = '0'.repeat(64);
    else if (stale) graphEvidence.sources[1].sha256 = '0'.repeat(64);
    const child = spawnSync(process.execPath, [probe], { input: JSON.stringify({ typescriptModule: compiler, config, calls: [call], graphEvidence }), encoding: 'utf8',
      env: { ...process.env, AUTOREVIEW_TYPESCRIPT_TRUST_EMITTED_GRAPH: trust ? '1' : '0', AUTOREVIEW_TYPESCRIPT_TRUST_NODE_12_22_12: node12 } });
    assert.equal(child.status, 0, child.stderr);
    const result = JSON.parse(child.stdout);
    return result.calls?.[0]?.status || result.status;
  }
  const source = "import * as lib from './library'; lib.store(); lib.makeUrl();";
  assert.equal(invoke(source, 'lib.store()', false), 'not_proven');
  assert.equal(invoke(source, 'lib.store()', true), 'not_promise');
  assert.equal(invoke(source, 'lib.store()', true, true), 'incomplete');
  assert.equal(invoke(source, 'lib.store()', true, 'options'), 'incomplete');
  assert.equal(invoke(source, 'lib.makeUrl()', true), 'not_promise');
  assert.equal(invoke("import * as lib from './library'; (lib as any).store = async () => {}; lib.store();", 'lib.store()', true), 'not_proven');
  fs.writeFileSync(library, librarySource + '\nstore = async () => {};');
  assert.equal(invoke(source, 'lib.store()', true), 'incomplete');
  fs.writeFileSync(library, librarySource + '\n(URL.prototype as any).then = () => {};');
  assert.equal(invoke(source, 'lib.makeUrl()', true), 'not_proven');
});

test('private injected receivers require a single closed-world concrete implementation', { skip: !compiler || !nodeTypes || !node12 }, t => {
  const { file, config } = probeFixture(t), root = path.dirname(file), library = path.join(root, 'library.ts');
  const typeDir = path.join(root, 'node_modules/@types/node');
  fs.mkdirSync(path.dirname(typeDir), { recursive: true });
  fs.cpSync(nodeTypes, typeDir, { recursive: true });
  fs.writeFileSync(config, JSON.stringify({ compilerOptions: { target: 'ES2020', lib: ['ES2020'], module: 'CommonJS', strict: true, skipLibCheck: true }, files: ['sample.ts', 'library.ts'] }));
  function librarySource(asyncMethod = false) {
    return `export interface Service { run(): void }
class Implementation { ${asyncMethod ? 'async run(): Promise<void>' : 'run(): void'} {} static async make(): Promise<Implementation> { const result = new Implementation(); return result; } }
export async function make(): Promise<Service> { return await Implementation.make(); }
class Holder { private readonly service: Service; constructor(service: Service) { this.service = service; } execute() { this.service.run(); } }
export function create(service: Service) { return new Holder(service); }`;
  }
  function invoke(app, source = librarySource(), trust = true) {
    fs.writeFileSync(file, app);
    fs.writeFileSync(library, source);
    const needle = 'this.service.run()', start = Buffer.byteLength(source.slice(0, source.indexOf(needle)));
    const call = { path: library, start, end: start + Buffer.byteLength(needle), source_sha256: createHash('sha256').update(source).digest('hex') };
    const graphEvidence = trust ? graphEvidenceFor(root, config, [file, library]) : undefined;
    const child = spawnSync(process.execPath, [probe], { input: JSON.stringify({ typescriptModule: compiler, config, calls: [call], graphEvidence }), encoding: 'utf8',
      env: { ...process.env, AUTOREVIEW_TYPESCRIPT_TRUST_EMITTED_GRAPH: trust ? '1' : '0', AUTOREVIEW_TYPESCRIPT_TRUST_NODE_12_22_12: node12 } });
    assert.equal(child.status, 0, child.stderr);
    const result = JSON.parse(child.stdout);
    return result.calls?.[0]?.status || result.status;
  }
  const app = "import * as lib from './library'; async function main() { const value = await lib.make(); lib.create(value).execute(); }";
  assert.equal(invoke(app, librarySource(), false), 'not_proven');
  assert.equal(invoke(app), 'not_promise');
  assert.equal(invoke(app + '\nlib.create({ run: async () => {} });'), 'not_proven');
  assert.equal(invoke(app, librarySource(true)), 'not_proven');
  assert.equal(invoke(app.replace('lib.create(value)', "Object.defineProperty(value, 'run', {value: async () => {}}); lib.create(value)")), 'not_proven');
  assert.equal(invoke(app, librarySource().replace('return new Holder(service);', 'service = { run: async () => {} }; return new Holder(service);')), 'not_proven');
  assert.equal(invoke(app, librarySource().replace('this.service = service;', 'service = { run: async () => {} }; this.service = service;')), 'not_proven');
  assert.equal(invoke(app.replace('lib.create(value)', "Object.defineProperty(value, 'then', {value: () => {}}); lib.create(value)")), 'not_proven');
  assert.equal(invoke(app + '\n(Promise.prototype as any).then = () => {};'), 'not_proven');
  assert.equal(invoke(app.replace('lib.create(value).execute();', "const detached = lib.create(value).execute; detached.call({service:{run:async () => {}}});")), 'not_proven');
  assert.equal(invoke(app.replace('lib.create(value).execute();', "const key: string = 'execute'; const detached = (lib.create(value) as any)[key]; detached.call({service:{run:async () => {}}});")), 'not_proven');
});

test('audited Node 12 contracts require pinned runtime, declarations, and unmodified bindings', { skip: !compiler || !nodeTypes || !node12 }, t => {
  const { file, config } = probeFixture(t);
  const typeDir = path.join(path.dirname(file), 'node_modules/@types/node');
  fs.mkdirSync(path.dirname(typeDir), { recursive: true });
  fs.cpSync(nodeTypes, typeDir, { recursive: true });
  fs.writeFileSync(config, JSON.stringify({ compilerOptions: { target: 'ES2020', lib: ['ES2020'], module: 'CommonJS', strict: true, skipLibCheck: true }, files: ['sample.ts', 'other.ts'] }));
  const other = path.join(path.dirname(file), 'other.ts');
  fs.writeFileSync(other, 'export {};\n');
  function invoke(source, runtime, needle) {
    fs.writeFileSync(file, source);
    const start = Buffer.byteLength(source.slice(0, source.lastIndexOf(needle)));
    const call = { path: file, start, end: start + Buffer.byteLength(needle), source_sha256: createHash('sha256').update(source).digest('hex') };
    const child = spawnSync(process.execPath, [probe], { input: JSON.stringify({ typescriptModule: compiler, config, calls: [call] }),
      encoding: 'utf8', env: { ...process.env, AUTOREVIEW_TYPESCRIPT_TRUST_NODE_12_22_12: runtime } });
    assert.equal(child.status, 0, child.stderr);
    return JSON.parse(child.stdout).calls[0].status;
  }
  const source = "import * as assert from 'assert'; import * as fs from 'fs'; let stats: fs.Stats; declare const text: string; Buffer.from('x'); setTimeout(() => {}, 1); assert.ok(true); fs.statSync('x'); text.match(/x/); text.split('x'); Object.keys({});";
  for (const needle of ["Buffer.from('x')", 'setTimeout(() => {}, 1)', 'assert.ok(true)', "fs.statSync('x')", 'text.match(/x/)', "text.split('x')", 'Object.keys({})']) {
    assert.equal(invoke(source, '', needle), 'not_proven');
    assert.equal(invoke(source, node12, needle), 'not_promise', needle);
    assert.equal(invoke(source, process.execPath, needle), 'not_proven');
  }
  assert.equal(invoke("(Buffer as any).from = async () => {}; Buffer.from('x');", node12, "Buffer.from('x')"), 'not_proven');
  assert.equal(invoke("(globalThis as any).setTimeout = async () => {}; setTimeout(() => {}, 1);", node12, 'setTimeout(() => {}, 1)'), 'not_proven');
  assert.equal(invoke("(String.prototype as any).match = async () => {}; 'x'.match(/x/);", node12, "'x'.match(/x/)"), 'not_proven');
  assert.equal(invoke("(RegExp.prototype as any)[Symbol.match] = async () => {}; 'x'.match(/x/);", node12, "'x'.match(/x/)"), 'not_proven');
  assert.equal(invoke("(Object as any).keys = async () => {}; Object.keys({});", node12, 'Object.keys({})'), 'not_proven');
  assert.equal(invoke("(Array.prototype as any).then = () => {}; 'x'.split('x');", node12, "'x'.split('x')"), 'not_proven');
  assert.equal(invoke("const value = {}; Object.defineProperty(value, 'then', {value: () => {}}); Object.keys({});", node12, 'Object.keys({})'), 'not_proven');
  assert.equal(invoke("const matcher = { [Symbol.match]() { return Promise.resolve(); } }; 'x'.match(matcher as any);", node12, "'x'.match(matcher as any)"), 'not_proven');
  fs.writeFileSync(other, "import * as assert from 'assert'; (assert as any).ok = async () => {};\n");
  assert.equal(invoke(source, node12, 'assert.ok(true)'), 'not_proven');
  fs.writeFileSync(other, "import * as fs from 'fs'; (fs as any).statSync = async () => {};\n");
  assert.equal(invoke(source, node12, "fs.statSync('x')"), 'not_proven');
  fs.writeFileSync(other, 'export {};\n');
  fs.appendFileSync(path.join(typeDir, 'globals.d.ts'), '\n// changed\n');
  assert.equal(invoke(source, node12, "Buffer.from('x')"), 'not_proven');
});

test('audited actions-core contracts require opt-in, exact files, and unmodified namespace imports', { skip: !compiler || !actionsCore }, t => {
  const { file, config } = probeFixture(t);
  const packageDir = path.join(path.dirname(file), 'node_modules/@actions/core');
  fs.mkdirSync(path.join(packageDir, 'lib'), { recursive: true });
  for (const name of ['core.js', 'core.d.ts', 'command.js', 'command.d.ts']) fs.copyFileSync(path.join(actionsCore, 'lib', name), path.join(packageDir, 'lib', name));
  fs.writeFileSync(path.join(packageDir, 'package.json'), JSON.stringify({ name: '@actions/core', main: 'lib/core.js', types: 'lib/core.d.ts' }));
  fs.writeFileSync(config, JSON.stringify({ compilerOptions: { target: 'ES2020', module: 'CommonJS', strict: true }, files: ['sample.ts', 'other.ts'] }));
  const other = path.join(path.dirname(file), 'other.ts');
  fs.writeFileSync(other, 'export {};\n');
  function invoke(source, trust) {
    fs.writeFileSync(file, source);
    const start = Buffer.byteLength(source.slice(0, source.lastIndexOf('core.info(')));
    const call = { path: file, start, end: start + Buffer.byteLength("core.info('hello')"), source_sha256: createHash('sha256').update(source).digest('hex') };
    const child = spawnSync(process.execPath, [probe], { input: JSON.stringify({ typescriptModule: compiler, config, calls: [call] }),
      encoding: 'utf8', env: { ...process.env, AUTOREVIEW_TYPESCRIPT_TRUST_ACTIONS_CORE_1_1_3: trust ? '1' : '0' } });
    assert.equal(child.status, 0, child.stderr);
    return JSON.parse(child.stdout).calls[0].status;
  }
  const plain = "import * as core from '@actions/core'; core.info('hello');";
  assert.equal(invoke(plain, false), 'not_proven');
  assert.equal(invoke(plain, true), 'not_promise');
  const commandSource = "import * as coreCommand from '@actions/core/lib/command'; coreCommand.issueCommand('x', {}, 'y');";
  fs.writeFileSync(file, commandSource);
  const commandStart = Buffer.byteLength(commandSource.slice(0, commandSource.indexOf('coreCommand.issueCommand')));
  const commandCall = { path: file, start: commandStart, end: commandStart + Buffer.byteLength("coreCommand.issueCommand('x', {}, 'y')"), source_sha256: createHash('sha256').update(commandSource).digest('hex') };
  const commandChild = spawnSync(process.execPath, [probe], { input: JSON.stringify({ typescriptModule: compiler, config, calls: [commandCall] }),
    encoding: 'utf8', env: { ...process.env, AUTOREVIEW_TYPESCRIPT_TRUST_ACTIONS_CORE_1_1_3: '1' } });
  assert.equal(commandChild.status, 0, commandChild.stderr);
  assert.equal(JSON.parse(commandChild.stdout).calls[0].status, 'not_promise');
  assert.equal(invoke("import * as core from '@actions/core'; (core as any).info = async () => {}; core.info('hello');", true), 'not_proven');
  fs.writeFileSync(other, "import * as core from '@actions/core'; (core as any).info = async () => {};\n");
  assert.equal(invoke(plain, true), 'not_proven');
  fs.writeFileSync(other, "import * as alternate from '@actions/core/lib/core'; (alternate as any).info = async () => {};\n");
  assert.equal(invoke(plain, true), 'not_proven');
  fs.writeFileSync(other, "declare const require: (name: string) => any; require('@actions/core/lib/core').info = async () => {};\n");
  assert.equal(invoke(plain, true), 'not_proven');
  fs.writeFileSync(other, 'export {};\n');
  fs.appendFileSync(path.join(packageDir, 'lib/core.js'), '\n// changed\n');
  assert.equal(invoke(plain, true), 'not_proven');
  fs.copyFileSync(path.join(actionsCore, 'lib/core.js'), path.join(packageDir, 'lib/core.js'));
  fs.appendFileSync(path.join(packageDir, 'lib/core.d.ts'), '\n// changed\n');
  assert.equal(invoke(plain, true), 'not_proven');
});

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
