import fc from 'fast-check';
const outcome = fc.check(fc.property(fc.nat(), n => n < 100));
if (outcome.failed) throw new Error('Property failed');
fc.assert(fc.property(fc.nat(), n => n >= 0));
