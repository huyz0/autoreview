class Cache {
    void put(Map<String, Integer> counts, String key) {
        counts.putIfAbsent(key, 0);
    }
}
