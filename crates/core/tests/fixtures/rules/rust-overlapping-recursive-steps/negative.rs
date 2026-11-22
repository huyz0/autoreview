fn estimate(n: u64) -> u64 { if n <= 1 { return n; } estimate(n - 1) + estimate(0) }
