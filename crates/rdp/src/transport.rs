//! Authenticated TLS transport. No accept-all certificate verifier is provided.
use crate::{Error, Result, crypto, invalid};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme,
    client::WebPkiServerVerifier,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::ring,
    pki_types::{CertificateDer, ServerName, UnixTime},
};

/// Unverified identity information for an explicit, connection-only trust decision.
#[derive(Clone, Debug)]
pub struct CertificateReview {
    pub host: String,
    pub subject: String,
    pub issuer: String,
    pub fingerprint: String,
    pub reason: String,
}
pub type ReviewCertificate = Arc<dyn Fn(CertificateReview) -> bool + Send + Sync>;
use std::{
    fmt,
    io::{self, Read, Write},
    net::TcpStream,
    sync::Arc,
};

pub enum Transport {
    Plain(TcpStream),
    Tls {
        socket: TcpStream,
        connection: Box<ClientConnection>,
    },
}
impl Transport {
    pub fn tls(socket: TcpStream, host: &str, pin: Option<[u8; 32]>) -> Result<Self> {
        Self::tls_with_review(socket, host, pin, None)
    }
    pub fn tls_with_review(
        socket: TcpStream,
        host: &str,
        pin: Option<[u8; 32]>,
        review: Option<ReviewCertificate>,
    ) -> Result<Self> {
        Self::tls_inner(socket, host, pin, review, None)
    }
    pub fn tls_with_review_traced(
        socket: TcpStream,
        host: &str,
        pin: Option<[u8; 32]>,
        review: Option<ReviewCertificate>,
        trace: &crate::trace::Trace,
    ) -> Result<Self> {
        Self::tls_inner(socket, host, pin, review, Some(trace))
    }
    fn tls_inner(
        socket: TcpStream,
        host: &str,
        pin: Option<[u8; 32]>,
        review: Option<ReviewCertificate>,
        trace: Option<&crate::trace::Trace>,
    ) -> Result<Self> {
        let provider = Arc::new(ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Security(e.to_string()))?;
        let config = if let Some(pin) = pin {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(Pinned { pin, provider }))
                .with_no_client_auth()
        } else {
            let mut roots = RootCertStore::empty();
            let roots_span = trace.map(|t| t.span("native_trust_store_load", "network", 0));
            let native = rustls_native_certs::load_native_certs();
            for cert in native.certs {
                let _ = roots.add(cert);
            }
            drop(roots_span);
            if roots.is_empty() {
                return Err(Error::Security(
                    "no system trust anchors; configure an explicit certificate SHA-256 pin".into(),
                ));
            }
            if let Some(review) = review {
                let verifier =
                    WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
                        .build()
                        .map_err(|e| Error::Security(e.to_string()))?;
                builder
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(Reviewed {
                        verifier,
                        review,
                        host: host.into(),
                    }))
                    .with_no_client_auth()
            } else {
                builder.with_root_certificates(roots).with_no_client_auth()
            }
        };
        let _span = trace.map(|t| t.span("tls_handshake", "network", 0));
        Self::tls_config(socket, host, Arc::new(config))
    }
    pub fn tls_config(
        mut socket: TcpStream,
        host: &str,
        config: Arc<ClientConfig>,
    ) -> Result<Self> {
        let name =
            ServerName::try_from(host.to_string()).map_err(|_| invalid("TLS server name"))?;
        let mut connection =
            ClientConnection::new(config, name).map_err(|e| Error::Security(e.to_string()))?;
        connection.set_buffer_limit(Some(65536));
        while connection.is_handshaking() {
            connection
                .complete_io(&mut socket)
                .map_err(|e| Error::Security(format!("TLS handshake: {e}")))?;
        }
        Ok(Self::Tls {
            socket,
            connection: Box::new(connection),
        })
    }
    pub fn certificate(&self) -> Result<&[u8]> {
        match self {
            Self::Tls { connection, .. } => connection
                .peer_certificates()
                .and_then(|c| c.first())
                .map(|c| c.as_ref())
                .ok_or_else(|| invalid("missing TLS peer certificate")),
            _ => Err(invalid("TLS certificate requested on plaintext transport")),
        }
    }
    pub fn public_key(&self) -> Result<Vec<u8>> {
        let (_, cert) = x509_parser::parse_x509_certificate(self.certificate()?)
            .map_err(|_| invalid("TLS certificate DER"))?;
        Ok(cert.public_key().subject_public_key.data.to_vec())
    }
    pub fn set_nonblocking(&self, value: bool) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.set_nonblocking(value),
            Self::Tls { socket, .. } => socket.set_nonblocking(value),
        }
    }
    fn drain(socket: &mut TcpStream, connection: &mut ClientConnection) -> io::Result<()> {
        while connection.wants_write() {
            if connection.write_tls(socket)? == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
        }
        Ok(())
    }
}
struct Reviewed {
    verifier: Arc<WebPkiServerVerifier>,
    review: ReviewCertificate,
    host: String,
}
impl fmt::Debug for Reviewed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReviewedCertificate")
    }
}
impl ServerCertVerifier for Reviewed {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        match self
            .verifier
            .verify_server_cert(end, intermediates, name, ocsp, now)
        {
            Ok(valid) => Ok(valid),
            Err(error) => {
                // Malformed certificates cannot be reviewed. Signature verification
                // remains mandatory even after accepting an untrusted identity.
                let (_, cert) =
                    x509_parser::parse_x509_certificate(end.as_ref()).map_err(|_| {
                        rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
                    })?;
                let review = CertificateReview {
                    host: self.host.clone(),
                    subject: cert.subject().to_string(),
                    issuer: cert.issuer().to_string(),
                    fingerprint: crypto::fingerprint(end.as_ref()),
                    reason: error.to_string(),
                };
                if (self.review)(review) {
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(error)
                }
            }
        }
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.verifier.verify_tls12_signature(m, c, d)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.verifier.verify_tls13_signature(m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.verifier.supported_verify_schemes()
    }
}
impl Read for Transport {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(out),
            Self::Tls { socket, connection } => {
                // Even when no new application bytes are queued, drain TLS records
                // produced by an earlier nonblocking write before waiting for input.
                match Self::drain(socket, connection) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
                loop {
                    match connection.reader().read(out) {
                        Ok(n) => return Ok(n),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(e) => return Err(e),
                    }
                    let n = connection.read_tls(socket)?;
                    connection
                        .process_new_packets()
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    if n == 0 {
                        return connection.reader().read(out);
                    }
                }
            }
        }
    }
}
impl Write for Transport {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(data),
            Self::Tls { socket, connection } => {
                Self::drain(socket, connection)?;
                let n = connection.writer().write(data)?;
                match Self::drain(socket, connection) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
                Ok(n)
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls { socket, connection } => Self::drain(socket, connection),
        }
    }
}
struct Pinned {
    pin: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}
impl fmt::Debug for Pinned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PinnedCertificate")
    }
}
impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if !crypto::equal(&self.pin, &crypto::sha256(&[end.as_ref()])) {
            return Err(rustls::Error::General(format!(
                "certificate pin mismatch; received SHA-256 {}",
                crypto::fingerprint(end.as_ref())
            )));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            m,
            c,
            d,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            m,
            c,
            d,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
