import java.lang.ref.WeakReference;
import java.util.HashMap;

class WeakRegistry {
    void attach(java.util.WeakHashMap<Object, Object> registry, Object key) {
        registry.put(key, new WeakReference<>(key));
    }

    void storeStrong(HashMap<Object, Object> registry, Object item) {
        registry.put(item, item);
    }
}
