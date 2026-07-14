fun f() {
    try {
        doThing()
    } catch (e: Exception) {
        throw CustomException(e)
    }
}
fun doThing() {}
