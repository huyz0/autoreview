class Main {
    fun handle(lock: Lock) {
        lock.lock()
        lock.unlock()
        return
    }
}
