import { Worker as Background } from 'node:worker_threads';

export function enqueue(script) {
  const task = new Background(script);
}
