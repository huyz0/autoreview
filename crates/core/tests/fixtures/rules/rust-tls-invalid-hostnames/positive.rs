fn connect() -> Result<native_tls::TlsConnector, native_tls::Error> {
    native_tls::TlsConnector::builder().danger_accept_invalid_hostnames(true).build()
}
