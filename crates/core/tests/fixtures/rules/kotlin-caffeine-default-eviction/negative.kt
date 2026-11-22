import com.github.benmanes.caffeine.cache.Caffeine
import java.time.Duration

fun recent() = Caffeine.newBuilder()
    .expireAfterWrite(Duration.ofMinutes(5))
    .build<String, Int>()
