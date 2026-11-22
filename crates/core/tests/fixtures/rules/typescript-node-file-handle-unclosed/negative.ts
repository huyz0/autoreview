import { open as createHandle } from 'fs/promises';

export async function closeImmediately(filename: string) {
  const opened = await createHandle(filename);
  const alias = opened;
  await alias.close();
}

export async function managedFile(path: string) {
  await using handle = await createHandle(path);
  return handle.stat();
}
