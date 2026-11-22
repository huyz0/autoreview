export function detach(): void {
  document.removeEventListener("click", function discard(event: MouseEvent) {
    event.preventDefault();
  }, true);
}
