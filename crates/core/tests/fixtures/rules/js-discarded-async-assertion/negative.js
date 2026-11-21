it("waits for rejection", () => {
  return expect(load()).rejects.toThrow("unavailable");
});

it("waits for resolution", async () => {
  await expect(load()).resolves.not.toBeNull();
});
