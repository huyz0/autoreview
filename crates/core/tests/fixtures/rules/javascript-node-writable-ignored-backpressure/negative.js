import { createWriteStream } from 'node:fs';
import { once } from 'node:events';

async function output(path, chunks) {
    const target = createWriteStream(path);
    for (const chunk of chunks) {
        if (!target.write(chunk)) await once(target, 'drain');
    }
    target.end();
}

function other(chunks) {
    const target = new Logger();
    for (const chunk of chunks) target.write(chunk);
}
