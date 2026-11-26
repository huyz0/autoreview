// Benchmark-only adapter: restore the TypeScript 3.6 catch-variable default
// while leaving all diagnostics, including declaration checks, enabled.
const path = require('node:path');
const compilerPath = process.env.AUTOREVIEW_BENCHMARK_TYPESCRIPT_MODULE;
if (!compilerPath || !path.isAbsolute(compilerPath)) throw new Error('Expected an absolute trusted compiler path');
const compiler = require(compilerPath);
const adapter = Object.create(compiler);
Object.defineProperty(adapter, 'parseJsonConfigFileContent', {
  value(...args) {
    const parsed = compiler.parseJsonConfigFileContent(...args);
    parsed.options.useUnknownInCatchVariables = false;
    return parsed;
  },
});
module.exports = adapter;
