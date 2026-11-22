import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.cancel

fun cancelAfterSetup() {
    val setup = CoroutineScope(Dispatchers.IO)
    setup.cancel()
}
