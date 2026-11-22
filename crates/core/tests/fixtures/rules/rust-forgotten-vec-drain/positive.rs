fn remove_first(items: &mut Vec<String>) {
    std::mem::forget(Vec::drain(items, ..1));
}
