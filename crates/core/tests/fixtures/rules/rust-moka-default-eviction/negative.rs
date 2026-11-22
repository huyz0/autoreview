use moka::future::Cache;

fn tags() -> Cache<String, String> {
    Cache::builder().max_capacity(500).build()
}
