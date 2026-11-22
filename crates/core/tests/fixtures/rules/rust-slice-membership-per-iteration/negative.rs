fn run(items: &[u64], allowed: &std::collections::HashSet<u64>) { for item in items { if allowed.contains(item) { println!("{item}"); } } }
