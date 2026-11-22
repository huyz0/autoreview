use moka::sync::Cache;

fn names() -> Cache<u64, String> {
    Cache::builder().build()
}
