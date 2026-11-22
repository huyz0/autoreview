import java.nio.file.Files as Nio
import java.nio.file.Path

fun countRows(path: Path): Long {
    val rows = Nio.lines(path)
    return rows.count()
}
