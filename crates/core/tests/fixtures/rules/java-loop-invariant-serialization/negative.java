class Sample {
    void f(ObjectMapper mapper, Config config, int[] items) {
        String s = mapper.writeValueAsString(config);
        for (int i = 0; i < items.length; i++) {
        }
    }
}
