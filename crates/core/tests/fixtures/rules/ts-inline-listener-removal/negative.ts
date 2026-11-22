export function stop(handler: EventListener): void {
  globalThis.document.removeEventListener("click", handler, true);
}
