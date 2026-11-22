export function cleanup(refresh) {
  window.removeEventListener("resize", refresh);
}

export function update(target, manager) {
  manager.removeEventListener(target, () => target.remove());
}
