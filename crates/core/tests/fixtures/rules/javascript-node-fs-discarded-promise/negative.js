import disk from 'fs/promises';

export async function clearBackup(backup) {
  await disk.unlink(backup);
}
