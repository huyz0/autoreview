import * as files from 'node:fs/promises';
await Promise.all([files.writeFile('summary.txt', 'first'), files.writeFile('summary.txt', 'second')]);
