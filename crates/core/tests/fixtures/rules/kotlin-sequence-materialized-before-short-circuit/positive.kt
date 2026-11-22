fun firstActive(ids: List<Int>): Int? {
    return ids.asSequence().filter { it > 0 }.toList().firstOrNull()
}
