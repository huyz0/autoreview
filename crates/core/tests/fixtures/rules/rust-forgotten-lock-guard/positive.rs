use std::sync::Mutex;

fn hold_lock(lock: &Mutex<u32>) {
    let guard: std::sync::MutexGuard<'_, u32> = lock.lock().unwrap();
    std::mem::forget(guard);
}
