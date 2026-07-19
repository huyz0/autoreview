suspend fun update(mutex: Mutex) {
    mutex.lock()
    state.value += 1
    mutex.unlock()
}
