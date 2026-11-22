export function prefix() {
  const packet = Buffer.alloc(500000);
  return packet.slice(0, 16);
}
