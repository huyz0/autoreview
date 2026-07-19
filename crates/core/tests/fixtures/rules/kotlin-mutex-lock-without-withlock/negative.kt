suspend fun update(mutex: Mutex) {
    mutex.withLock {
        state.value += 1
    }
}
