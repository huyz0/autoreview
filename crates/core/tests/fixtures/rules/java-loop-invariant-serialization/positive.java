class Sample {
    void f(ObjectMapper mapper, Config config, int[] items) {
        for (int i = 0; i < items.length; i++) {
            String s = mapper.writeValueAsString(config);
        }
    }
}
