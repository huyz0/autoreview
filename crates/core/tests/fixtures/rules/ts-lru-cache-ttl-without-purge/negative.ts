import { LRUCache } from "lru-cache";

export const prices = new LRUCache<string, number>({ ttl: 30_000, maxSize: 1024, sizeCalculation: () => 8 });
