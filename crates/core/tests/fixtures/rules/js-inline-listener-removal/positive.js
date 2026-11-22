export function disconnect() {
  window.removeEventListener("resize", () => updateDimensions());
}
