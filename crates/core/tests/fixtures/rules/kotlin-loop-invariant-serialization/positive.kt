class Sample {
    fun f(mapper: ObjectMapper, config: Config, items: List<Int>) {
        for (item in items) {
            val s = mapper.writeValueAsString(config)
        }
    }
}
