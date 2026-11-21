test("reports the result", () => {
  expect(load()).resolves.toEqual({ status: "ready" });
});
