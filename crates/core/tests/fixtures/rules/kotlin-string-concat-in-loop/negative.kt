class Sample {
    fun f(items: List<String>): String {
        val b = StringBuilder()
        for (item in items) {
            b.append(item)
        }
        return b.toString()
    }
}
