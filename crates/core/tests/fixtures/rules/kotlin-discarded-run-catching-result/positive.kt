import kotlin.runCatching as attempt

fun loadSetting(value: String) {
    attempt { value.toInt() }
}
