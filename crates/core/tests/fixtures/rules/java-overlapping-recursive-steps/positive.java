class Sample { int estimate(int n) { if (n <= 1) return n; return estimate(n - 1) + estimate(n - 2); } }
