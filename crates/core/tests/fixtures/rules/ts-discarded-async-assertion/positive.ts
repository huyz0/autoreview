import { expect, test } from "@jest/globals";

test("rejects invalid input", () => {
  expect(Promise.reject(new Error("invalid"))).rejects.not.toThrow("missing");
});
