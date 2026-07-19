function get(m: Map<string, number>, key: string): number {
  const v = m.get(key);
  if (v === undefined) throw new Error("missing");
  return v;
}
