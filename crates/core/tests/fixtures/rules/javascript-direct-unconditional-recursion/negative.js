function run(x) { if (x <= 0) return x; return run(x - 1); }
function apply(apply) { return apply(apply); }
function other(x) { return run(x); }
