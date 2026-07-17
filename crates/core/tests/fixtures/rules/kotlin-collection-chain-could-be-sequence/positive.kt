class Sample {
    fun f(items: List<Int>): List<Int> {
        return items.filter { it > 0 }.map { it * 2 }
    }
}
