import { LRUCache } from "lru-cache";

export const responses = new LRUCache({ ttl: 60_000, ttlAutopurge: false });
