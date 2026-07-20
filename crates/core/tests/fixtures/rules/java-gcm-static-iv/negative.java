class C {
    void b(byte[] freshNonce) {
        GCMParameterSpec spec = new GCMParameterSpec(128, freshNonce);
    }
}
