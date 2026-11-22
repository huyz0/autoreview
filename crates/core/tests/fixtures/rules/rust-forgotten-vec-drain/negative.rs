fn remove_first(items: &mut Vec<String>) {
    drop(Vec::drain(items, ..1));
}

fn preserve_first(items: &mut Vec<String>) -> Option<String> {
    items.drain(..1).next()
}
