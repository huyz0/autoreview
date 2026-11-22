function countReady(queue: readonly string[]): number {
  return queue.filter(job => job === "ready").length;
}

function hasReady(queue: readonly string[]): boolean {
  return queue.some(job => job === "ready");
}
