export function isMissing(values, wanted) {
  return values.filter(value => value === wanted).length === 0;
}
