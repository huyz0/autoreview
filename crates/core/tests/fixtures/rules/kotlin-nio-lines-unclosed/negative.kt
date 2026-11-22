import java.nio.file.Path

fun summarize(path: Path): Long {
    val stream = java.nio.file.Files.lines(path)
    return stream.use { it.count() }
}
