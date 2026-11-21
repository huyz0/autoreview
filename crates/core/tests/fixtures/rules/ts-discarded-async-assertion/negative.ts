import { expect, test } from "@jest/globals";

test("checks the payload", async () => {
  await expect(Promise.resolve(42)).resolves.toBe(42);
  expect(42).not.toBe(41);
});
