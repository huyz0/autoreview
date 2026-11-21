async function refresh(tasks) {
  await Promise.all(tasks);
  Promise.any(tasks).catch(reportError);
  const pending = Promise.allSettled(tasks);
  await pending;
  return Promise.race(tasks);
}
function start(tasks) {
  void Promise.all(tasks);
  queue.all(tasks);
}
