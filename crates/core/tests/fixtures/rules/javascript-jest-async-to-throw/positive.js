import { expect, test } from '@jest/globals';
test('rejects', () => { expect(async () => { throw new Error('bad'); }).toThrow(); });
