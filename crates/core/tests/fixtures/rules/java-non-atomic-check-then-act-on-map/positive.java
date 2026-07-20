class Cache {
    void put(Map<String, Integer> counts, String key) {
        if (!counts.containsKey(key)) {
            counts.put(key, 0);
        }
    }
}
