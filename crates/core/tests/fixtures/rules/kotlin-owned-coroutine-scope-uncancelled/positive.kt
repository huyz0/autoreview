import kotlinx.coroutines.CoroutineScope as createScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

fun submitExport(work: () -> Unit) {
    val owner = createScope(Dispatchers.Default)
    owner.launch { work() }
}
