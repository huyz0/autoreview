class Sample {
    fun f(items: List<Int>): List<Int> {
        return items.asSequence().filter { it > 0 }.map { it * 2 }.toList()
    }
}
