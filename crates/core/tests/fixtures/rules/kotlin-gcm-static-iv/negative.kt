class C {
    fun b(freshNonce: ByteArray) {
        val spec = GCMParameterSpec(128, freshNonce)
    }
}
