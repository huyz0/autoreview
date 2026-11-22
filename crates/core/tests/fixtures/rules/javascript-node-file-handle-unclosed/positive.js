import { open as acquire } from 'fs/promises';

export async function touch(filename) {
  const token = await acquire(filename, 'a');
  token.close();
}
