import java.time.format.DateTimeFormatter

fun format() = DateTimeFormatter.ofPattern("uuuu-MM-dd")
fun week() = DateTimeFormatter.ofPattern("YYYY-ww-e")
