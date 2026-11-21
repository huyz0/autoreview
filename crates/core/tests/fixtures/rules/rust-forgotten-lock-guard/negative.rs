use std::sync::{Mutex, RwLock};

fn drop_guard(lock: &RwLock<u32>) {
    let guard: std::sync::RwLockReadGuard<'_, u32> = lock.read().unwrap();
    std::mem::drop(guard);
}

fn forget_reference(lock: &Mutex<u32>) {
    let guard: std::sync::MutexGuard<'_, u32> = lock.lock().unwrap();
    std::mem::forget(&guard);
}

fn forget_shadow(lock: &RwLock<u32>) {
    let guard: std::sync::RwLockWriteGuard<'_, u32> = lock.write().unwrap();
    let guard = guard.to_string();
    std::mem::forget(guard);
}

fn forget_other(lock: &Mutex<u32>, other: String) {
    let _guard: std::sync::MutexGuard<'_, u32> = lock.lock().unwrap();
    std::mem::forget(other);
}
