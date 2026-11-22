import * as disk from 'node:fs/promises';

export async function reserve(location: string): Promise<void> {
  const reservation = await disk.open(location, 'w');
}
