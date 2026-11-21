import org.mockito.Mockito

fun check(worker: Runnable) {
    Mockito.verify(worker)
}
