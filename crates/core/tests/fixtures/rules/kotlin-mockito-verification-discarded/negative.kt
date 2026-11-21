import org.mockito.Mockito

fun check(worker: Runnable) {
    Mockito.verify(worker).run()
    org.mockito.Mockito.verify(worker, Mockito.never()).run()
    CustomVerifier.verify(worker)
    val verification = run { Mockito.verify(worker) }
    verification.run()
}
