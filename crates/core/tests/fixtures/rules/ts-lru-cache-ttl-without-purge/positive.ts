import { LRUCache } from "lru-cache";

export const users = new LRUCache<string, number>({ ttl: 30_000, allowStale: true });
