import * as files from 'node:fs/promises';
await files.writeFile('summary.txt', 'first');
await files.writeFile('summary.txt', 'second');
await Promise.all([files.writeFile('one.txt', 'first'), files.writeFile('two.txt', 'second')]);
