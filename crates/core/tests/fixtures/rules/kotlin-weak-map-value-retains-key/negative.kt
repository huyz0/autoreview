import java.lang.ref.WeakReference
import java.util.WeakHashMap

fun holdWeakly(cache: WeakHashMap<Any, Any>, record: Any) {
    cache.put(record, WeakReference(record))
}

fun keepIdentity(cache: MutableMap<Any, Any>, record: Any) {
    cache.put(record, record)
}
