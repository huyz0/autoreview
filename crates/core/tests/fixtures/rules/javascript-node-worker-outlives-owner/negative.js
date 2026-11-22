import * as jobs from 'node:worker_threads';

export async function stopSmallJob(source) {
  const worker = new jobs.Worker(source);
  await worker.terminate();
}
