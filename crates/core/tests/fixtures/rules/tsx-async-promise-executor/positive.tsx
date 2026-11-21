export function View() {
  const result = new Promise<number>(async function fetchValue(resolve) {
    resolve(await loadValue());
  });
  return <Status value={result} />;
}
