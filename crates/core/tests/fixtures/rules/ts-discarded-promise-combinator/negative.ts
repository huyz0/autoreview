function handleFailures(jobs: Promise<number>[], onError: (error: unknown) => void) {
  Promise.any(jobs).catch(onError);
}

async function collect(jobs: Promise<number>[]) {
  const results = await Promise.allSettled(jobs);
  return [results, await Promise.all(jobs)];
}

function first(jobs: Promise<number>[]) {
  return Promise.race(jobs);
}
