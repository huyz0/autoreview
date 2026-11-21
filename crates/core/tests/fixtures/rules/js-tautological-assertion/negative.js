test("compares the result", () => {
  const actual = calculate();
  expect(actual).toEqual(42);
  expect(read()).toBe(read());
});
