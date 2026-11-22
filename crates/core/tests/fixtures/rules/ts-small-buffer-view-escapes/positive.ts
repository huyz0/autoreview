import { Buffer } from "node:buffer";

export function header(): Buffer {
  const payload = Buffer.alloc(1048576);
  return payload.subarray(0, 32);
}
