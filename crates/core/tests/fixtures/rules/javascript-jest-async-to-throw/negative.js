import { expect, test } from '@jest/globals';
test('fails', async () => {
  expect(() => { throw new Error('bad'); }).toThrow();
  await expect(Promise.reject(new Error('bad'))).rejects.toThrow();
});
