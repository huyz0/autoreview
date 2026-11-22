fn run(items: &[u64], allowed: &[u64]) { for item in items { if allowed.contains(item) { println!("{item}"); } } }
