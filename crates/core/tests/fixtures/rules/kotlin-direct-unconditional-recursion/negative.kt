fun run(x: Int): Int { if (x <= 0) return x; return run(x - 1) }
fun other(x: Int): Int = run(x)
fun invoke(invoke: (Any) -> Any): Any = invoke(invoke)
