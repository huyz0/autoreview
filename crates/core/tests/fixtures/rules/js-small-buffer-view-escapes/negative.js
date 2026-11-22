export function localSample() {
  const buffer = Buffer.alloc(64);
  return buffer.subarray(0, 32);
}
