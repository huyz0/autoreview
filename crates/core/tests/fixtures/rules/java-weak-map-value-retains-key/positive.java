import java.util.WeakHashMap;

class WeakRegistry {
    void register(WeakHashMap<Object, Object> registry, Object key) {
        registry.put(key, key);
    }
}
