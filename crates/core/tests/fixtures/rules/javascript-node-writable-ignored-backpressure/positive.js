import { createWriteStream } from 'node:fs';

function output(path, chunks) {
    const target = createWriteStream(path);
    for (const chunk of chunks) {
        target.write(chunk);
    }
    target.end();
}
