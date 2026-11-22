import { rename as move } from 'node:fs/promises';

export function promote(staged, finalPath) {
  move(staged, finalPath);
}
