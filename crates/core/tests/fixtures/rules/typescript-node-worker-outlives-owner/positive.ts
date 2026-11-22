import * as workers from 'node:worker_threads';

export function scheduleJob(script: string): void {
  new workers.Worker(script);
}
