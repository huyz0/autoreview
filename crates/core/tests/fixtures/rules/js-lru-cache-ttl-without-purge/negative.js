import { LRUCache } from "lru-cache";

export const profiles = new LRUCache({ ttl: 60_000, max: 500 });
export const shortLived = new LRUCache({ ttl: 10_000, ttlAutopurge: true });
export function build(options) {
  return new LRUCache({ ttl: 1000, ...options });
}
