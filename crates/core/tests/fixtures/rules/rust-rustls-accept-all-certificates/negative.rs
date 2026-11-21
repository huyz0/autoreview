impl rustls::client::danger::ServerCertVerifier for CustomVerifier {
    fn verify_server_cert(
        &self,
        certificate: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        self.verifier.verify_server_cert(certificate, intermediates, name, ocsp_response, now)
    }
}
