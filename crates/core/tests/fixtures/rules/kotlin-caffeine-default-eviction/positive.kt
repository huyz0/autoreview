import com.github.benmanes.caffeine.cache.Caffeine

fun recent() = Caffeine.newBuilder().build<String, Int>()
