class Sample { int run(int x) { if (x <= 0) return x; return run(x - 1); } int other(int x) { return run(x); } }
