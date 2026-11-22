import kotlin.runCatching

class ParsedSlot {
    var result: Result<Int>? = null
}

fun abandoned(text: String) {
    val outcome = runCatching { text.toInt() } // unused result; then outcome.getOrThrow()
    val callback = { println(outcome.getOrThrow()) }
}

fun overwrittenField(text: String): Result<Int>? {
    val box = ParsedSlot()
    box.result = runCatching { text.toInt() } // unused result; then box.result?.getOrThrow()
    box.result = null
    return box.result
}

fun throughCallback(text: String): Result<Int> {
    val parsed = runCatching { text.toInt() }
    val read = { parsed }
    return read()
}

fun laterBinding(text: String) {
    var first = runCatching { text.toInt() } // unused result; then first.getOrThrow()
    val consume = { println(first.getOrThrow()) }
    first = runCatching { 0 }
    consume()
}

fun fieldReadInClosure(text: String): Result<Int>? {
    val slot = ParsedSlot()
    slot.result = runCatching { text.toInt() }
    val retrieve = { slot.result }
    return retrieve()
}
