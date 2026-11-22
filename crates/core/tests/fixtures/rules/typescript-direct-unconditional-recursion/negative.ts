function run(x: number): number { if (x <= 0) return x; return run(x - 1); }
function other(x: number): number { return run(x); }
function apply(apply: (f: unknown) => number): number { return apply(apply); }
