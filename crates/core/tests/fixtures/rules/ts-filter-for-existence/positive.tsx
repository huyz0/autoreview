export function QueueSummary({ states }: { states: readonly string[] }) {
  const active = states.filter(state => state === "active").length > 0;
  return <output>{active ? "active" : "idle"}</output>;
}
