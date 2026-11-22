fun parseSetting(value: String): Result<Int> = kotlin.runCatching { value.toInt() }
