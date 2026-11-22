export function hasName(values, name) {
  return values.some(value => value === name);
}

export function findMatches(values, name) {
  return values.filter(value => value === name);
}
