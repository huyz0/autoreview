import { Worker as ServiceWorker } from 'node:worker_threads';

export function createWorker(entry: URL): ServiceWorker {
  return new ServiceWorker(entry);
}
