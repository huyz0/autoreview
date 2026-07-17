class Sample {
    fun f(mapper: ObjectMapper, config: Config, items: List<Int>) {
        val s = mapper.writeValueAsString(config)
        for (item in items) {
        }
    }
}
