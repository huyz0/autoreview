fn run(x: u64) -> u64 { if x == 0 { return x; } run(x - 1) }
struct Demo;
impl Demo { fn run(x: u64) -> u64 { run(x) } }
