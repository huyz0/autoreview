import * as disk from 'node:fs/promises';

export function removeExpired(path: string): void {
  disk.unlink(path);
}
