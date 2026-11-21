import kotlin.test.assertEquals

fun checkResult(expected: Int, actual: Int) {
    assertEquals(expected, actual)
    assertEquals(read(), read())
}
