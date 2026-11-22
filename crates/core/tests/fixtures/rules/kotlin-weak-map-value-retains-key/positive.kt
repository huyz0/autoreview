import java.util.WeakHashMap

fun remember(cache: WeakHashMap<Any, Any>, key: Any) {
    cache.put(key, key)
}
