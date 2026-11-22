fun snapshot(ids: List<Int>): List<Int> = ids.asSequence().filter { it > 0 }.toList()

fun hasActive(ids: List<Int>): Boolean = ids.asSequence().filter { it > 0 }.any()
