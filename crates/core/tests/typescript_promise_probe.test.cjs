const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { createHash } = require('node:crypto');

const compiler = process.env.AUTOREVIEW_TEST_TYPESCRIPT;
const probe = path.resolve(__dirname, '../src/analyzers/typescript_promise_probe.cjs');

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
      "const accent = 'é';",
      'const localGlobal = LOCAL_GLOBAL;',
      'promise(); sync(); anything(); mixed(); thenable();',
      'function generic<T>(value: T) { return identity(value); }',
      'declare function identity<T>(value: T): T;',
    ].join('\n');
    fs.writeFileSync(file, source);
    const names = ['promise()', 'sync()', 'anything()', 'mixed()', 'thenable()', 'identity(value)'];
    const hash = text => createHash('sha256').update(text, 'utf8').digest('hex');
    let calls = names.map(name => ({ path: file, start: Buffer.byteLength(source.slice(0, source.lastIndexOf(name))), source_sha256: hash(source) }));
    function invoke() {
      const child = spawnSync(process.execPath, [probe], { input: JSON.stringify({ typescriptModule: compiler, config, calls }), encoding: 'utf8' });
      assert.equal(child.status, 0, child.stderr);
      return JSON.parse(child.stdout);
    }
    const checked = invoke();
    assert.equal(checked.status, 'ok');
    assert.deepEqual(checked.calls.map(call => call.status), ['promise_like', 'not_promise', 'not_proven', 'not_proven', 'promise_like', 'not_proven']);
    const shadowed = 'export {}; type Promise<T> = string; declare function fake(): Promise<void>; fake();';
    fs.writeFileSync(file, shadowed);
    calls = [{ path: file, start: Buffer.byteLength(shadowed.slice(0, shadowed.lastIndexOf('fake()'))), source_sha256: hash(shadowed) }];
    assert.equal(invoke().calls[0].status, 'not_promise');
    calls[0].source_sha256 = hash(source);
    assert.equal(invoke().calls[0].status, 'source_changed');
    fs.appendFileSync(file, '\nconst broken: number = "wrong";\n');
    assert.deepEqual(invoke(), { status: 'incomplete', reason: 'project_type_errors' });
  } finally {
    const target = path.resolve(directory);
    const root = path.resolve(os.tmpdir()) + path.sep;
    if (!target.startsWith(root) || !path.basename(target).startsWith('autoreview-ts-probe-')) throw new Error('Unexpected temporary directory');
    fs.rmSync(target, { recursive: true, force: true });
  }
});
