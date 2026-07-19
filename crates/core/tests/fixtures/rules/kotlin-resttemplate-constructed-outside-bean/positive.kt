class Sample {
    fun fetch(id: String): Widget {
        val rt = RestTemplate()
        return rt.getForObject("/widgets/$id", Widget::class.java)
    }
}
