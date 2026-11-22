fun estimate(n: Int): Int { if (n <= 1) return n; return estimate(n - 1) + estimate(n - 2) }
