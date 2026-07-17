class Sample {
    fun f(items: List<String>): String {
        var s = ""
        for (item in items) {
            s = s + item
        }
        return s
    }
}
