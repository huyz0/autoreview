export function View() {
  const result = loadValue();
  return <Status value={result} />;
}
