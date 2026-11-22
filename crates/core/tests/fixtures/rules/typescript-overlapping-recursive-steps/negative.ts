function estimate(n: number): number { if (n <= 1) return n; return estimate(n - 1) + estimate(0); }
