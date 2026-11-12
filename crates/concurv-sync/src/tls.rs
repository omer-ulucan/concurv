//! TLS 1.3 with a pinned relay certificate (RFC 0003 §3.1).
//!
//! A relay generates a self-signed certificate at `concurvd init`. Clients
//! do not use certificate authorities or host names: they pin the SHA-256
//! fingerprint of the relay's certificate, given to them out of band when
//! the remote is added. The handshake signature is still verified, so only
//! the holder of the relay's private key can complete it. The client sends
//! its device token only after the handshake has succeeded.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use concurv_core::{ConcurvError, ErrorCode, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};

/// Name in the relay certificate and in the client's SNI. Not checked:
/// the pinned fingerprint is what identifies a relay.
pub const SERVER_NAME: &str = "concurv-relay";

/// A relay's certificate and private key (DER).
#[derive(Clone)]
pub struct Identity {
    /// Self-signed X.509 certificate.
    pub cert: Vec<u8>,
    /// PKCS#8 private key.
    pub key: Vec<u8>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("fingerprint", &fingerprint(&self.cert))
            .finish_non_exhaustive()
    }
}

fn tls_error(e: impl std::fmt::Display) -> ConcurvError {
    ConcurvError::internal(format!("TLS setup failed: {e}"))
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Generate a new relay identity (ECDSA P-256).
pub fn generate_identity() -> Result<Identity> {
    let ck = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_owned()]).map_err(tls_error)?;
    Ok(Identity {
        cert: ck.cert.der().to_vec(),
        key: ck.signing_key.serialize_der(),
    })
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let d = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

/// Fingerprint of a certificate: `sha256:` and 64 lowercase hex digits.
pub fn fingerprint(cert: &[u8]) -> String {
    let mut s = String::from("sha256:");
    for b in sha256(cert) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Parse a fingerprint as printed by [`fingerprint`] (uppercase hex and
/// `:` separators between bytes are accepted too).
pub fn parse_fingerprint(s: &str) -> Result<[u8; 32]> {
    let bad = || {
        ConcurvError::new(
            ErrorCode::InvalidArgument,
            format!(
                "invalid relay fingerprint {s:?}: expected `sha256:` and 64 hex digits (shown by `concurvd fingerprint`)"
            ),
        )
    };
    let hex: String = s
        .strip_prefix("sha256:")
        .ok_or_else(bad)?
        .chars()
        .filter(|c| *c != ':')
        .collect::<String>()
        .to_ascii_lowercase();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
    }
    Ok(out)
}

/// TLS configuration of a relay.
pub fn server_config(id: &Identity) -> Result<Arc<ServerConfig>> {
    let cfg = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_error)?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(id.cert.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key.clone())),
        )
        .map_err(tls_error)?;
    Ok(Arc::new(cfg))
}

#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; 32],
    provider: Arc<CryptoProvider>,
    mismatch: Arc<AtomicBool>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        // Certificates are public: no constant-time comparison needed.
        if sha256(end_entity.as_ref()) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            self.mismatch.store(true, Ordering::SeqCst);
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// TLS configuration of a client pinning `fingerprint`. The returned flag
/// is set if a handshake failed because the relay presented another
/// certificate.
pub fn client_config(fingerprint: &[u8; 32]) -> Result<(Arc<ClientConfig>, Arc<AtomicBool>)> {
    let provider = provider();
    let mismatch = Arc::new(AtomicBool::new(false));
    let verifier = Pinned {
        fingerprint: *fingerprint,
        provider: provider.clone(),
        mismatch: mismatch.clone(),
    };
    let cfg = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_error)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok((Arc::new(cfg), mismatch))
}

/// The server name a client presents.
pub fn server_name() -> Result<ServerName<'static>> {
    ServerName::try_from(SERVER_NAME).map_err(tls_error)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn fingerprints_parse_back() {
        let id = generate_identity().unwrap();
        let fp = fingerprint(&id.cert);
        assert!(fp.starts_with("sha256:") && fp.len() == 7 + 64);
        assert_eq!(parse_fingerprint(&fp).unwrap(), sha256(&id.cert));
        let upper_colons: String = {
            let hex = &fp[7..];
            let pairs: Vec<String> = (0..32)
                .map(|i| hex[2 * i..2 * i + 2].to_ascii_uppercase())
                .collect();
            format!("sha256:{}", pairs.join(":"))
        };
        assert_eq!(parse_fingerprint(&upper_colons).unwrap(), sha256(&id.cert));
        for bad in ["", "sha256:", "sha1:00", &fp[..fp.len() - 1], "sha256:zz"] {
            assert!(parse_fingerprint(bad).is_err(), "{bad}");
        }
        server_config(&id).unwrap();
        client_config(&sha256(&id.cert)).unwrap();
    }
}
