import com.github.benmanes.caffeine.cache.Caffeine;

class PriceCache {
    Object initialize() {
        return Caffeine.newBuilder().recordStats().build();
    }
}
