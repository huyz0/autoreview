import * as disk from 'node:fs/promises';

export async function readHeader(location) {
  const file = await disk.open(location);
  try {
    return await file.readFile();
  } finally {
    await file.close();
  }
}
