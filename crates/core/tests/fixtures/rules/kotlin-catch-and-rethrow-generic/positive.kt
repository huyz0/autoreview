fun f() {
    try {
        doThing()
    } catch (e: Exception) {
        throw RuntimeException(e)
    }
}
fun doThing() {}
