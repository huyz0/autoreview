import com.github.benmanes.caffeine.cache.Caffeine;

class BoundedProductCache {
    Object cache() {
        return Caffeine.newBuilder().maximumSize(500).recordStats().build();
    }

    Object custom() {
        return OtherCache.newBuilder().build();
    }
}
