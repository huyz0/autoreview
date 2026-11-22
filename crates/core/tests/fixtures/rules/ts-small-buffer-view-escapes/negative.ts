import { Buffer } from "node:buffer";

export function header(): Buffer {
  const packet = Buffer.alloc(1048576);
  return Buffer.from(packet.subarray(0, 32));
}
