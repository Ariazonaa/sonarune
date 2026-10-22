//! Per-server certificate trust.
//!
//! Instead of blanket-accepting invalid certificates, connections are validated
//! by the operating system's own certificate verifier (the normal, public-CA
//! path) and, additionally, against explicit SHA-256 fingerprints the user has
//! confirmed for a given server. That lets self-signed home servers work
//! without disabling all verification: only the exact certificate the user
//! pinned is accepted, and a silently changed certificate is rejected until
//! re-confirmed.
//!
//! The custom [`ServerCertVerifier`] records the fingerprint of every leaf it is
//! shown, so the caller can read back which certificate a failed handshake
//! presented and offer it for confirmation — no second probe connection needed.

use crate::error::{AppError, AppResult};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore,
    SignatureScheme,
};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex, OnceLock};

/// The fingerprint observed on the most recent handshake, shared with the
/// [`crate::api::JellyfinClient`] that owns the reqwest client.
pub type ObservedFingerprint = Arc<Mutex<Option<String>>>;

/// Colon-separated uppercase hex SHA-256 of a DER-encoded certificate, e.g.
/// `AB:CD:…`. This is the canonical stored/displayed form.
pub fn fingerprint(cert_der: &[u8]) -> String {
    Sha256::digest(cert_der)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Strip separators and case so two fingerprints compare equal regardless of
/// how they were entered/stored.
pub fn normalize(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .flat_map(|c| c.to_uppercase())
        .collect()
}

/// True when `a` and `b` denote the same certificate fingerprint.
pub fn matches(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

/// A SHA-256 fingerprint in one of the accepted spellings: exactly 64 hex
/// digits, optionally separated by `:`, `-` or spaces. Anything else is
/// rejected rather than normalized — `normalize` alone would turn arbitrary
/// text with 64 hex digits somewhere in it into a valid-looking pin.
pub fn is_valid_fingerprint(raw: &str) -> bool {
    raw.chars()
        .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '-' | ' '))
        && normalize(raw).len() == 64
}

#[derive(Debug)]
struct PinningVerifier {
    /// Normal verification of public certificates by the OS ([`os_verifier`]).
    /// `None` when the OS has no trust anchors at all (a Linux/macOS system
    /// without a CA bundle): then only pinned certificates can pass.
    inner: Option<Arc<dyn ServerCertVerifier>>,
    /// Signature algorithms for the handshake when there is no `inner`.
    provider: Arc<CryptoProvider>,
    /// User-confirmed fingerprints for this server, normalized.
    trusted: Vec<String>,
    /// Fingerprint of the last leaf certificate this verifier was shown.
    observed: ObservedFingerprint,
}

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let canonical = fingerprint(end_entity.as_ref());
        let pinned = self.trusted.iter().any(|t| *t == normalize(&canonical));

        let Some(inner) = &self.inner else {
            // Nothing public to validate against: record the fingerprint for
            // the trust prompt and accept only a pinned leaf.
            *self.observed.lock().unwrap() = Some(canonical);
            return if pinned {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(TlsError::InvalidCertificate(
                    CertificateError::UnknownIssuer,
                ))
            };
        };
        match inner.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now) {
            Ok(verified) => {
                // Publicly valid: there is nothing for the user to confirm.
                // Clearing matters — a leftover fingerprint would make the
                // next unrelated network failure (server down, DNS, timeout)
                // read as "untrusted certificate" and raise a trust prompt for
                // a certificate that is perfectly fine.
                *self.observed.lock().unwrap() = None;
                Ok(verified)
            }
            // Public validation failed (self-signed / unknown CA). Record the
            // fingerprint so the caller can offer it for confirmation, and
            // accept only if the user pinned this exact leaf certificate.
            //
            // Note the pinned branch deliberately does not re-check the host
            // name or validity dates: pins are stored per `server_url`, so a
            // pin can only ever be presented for the server it was confirmed
            // on, and self-signed home servers routinely have no matching SAN
            // (reached by IP, or a bare CN) — which is exactly the case this
            // feature exists to support.
            Err(err) => {
                *self.observed.lock().unwrap() = Some(canonical);
                if pinned {
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(err)
                }
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        match &self.inner {
            Some(inner) => inner.verify_tls12_signature(message, cert, dss),
            None => rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.provider.signature_verification_algorithms,
            ),
        }
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        match &self.inner {
            Some(inner) => inner.verify_tls13_signature(message, cert, dss),
            None => rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.provider.signature_verification_algorithms,
            ),
        }
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        match &self.inner {
            Some(inner) => inner.supported_verify_schemes(),
            None => self
                .provider
                .signature_verification_algorithms
                .supported_schemes(),
        }
    }
}

/// The crypto provider of every client, created once. Matches reqwest's
/// rustls backend (aws-lc-rs) so there is one provider in the process and no
/// default-provider ambiguity.
fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .clone()
}

/// The OS trust store, loaded once per process. Only the fallback verifier
/// reads it (see [`os_verifier`]).
fn root_store() -> Arc<RootCertStore> {
    static ROOTS: OnceLock<Arc<RootCertStore>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut roots = RootCertStore::empty();
            let result = rustls_native_certs::load_native_certs();
            for cert in result.certs {
                // A single malformed OS cert must not sink the whole store.
                let _ = roots.add(cert);
            }
            if roots.is_empty() {
                tracing::warn!("no OS trust anchors found; only pinned certificates are accepted");
            }
            Arc::new(roots)
        })
        .clone()
}

/// webpki over `roots`, or `None` when there is nothing to verify against:
/// webpki refuses to build over an empty store.
fn webpki_verifier(
    roots: Arc<RootCertStore>,
    provider: Arc<CryptoProvider>,
) -> Result<Option<Arc<dyn ServerCertVerifier>>, String> {
    if roots.is_empty() {
        return Ok(None);
    }
    let verifier = WebPkiServerVerifier::builder_with_provider(roots, provider)
        .build()
        .map_err(|e| format!("cannot build certificate verifier: {e}"))?;
    Ok(Some(verifier as Arc<dyn ServerCertVerifier>))
}

/// How public certificates are checked: by the operating system's own
/// verifier (rustls-platform-verifier), built once per process. On Windows
/// that is CryptoAPI's chain engine, which the rest of the system trusts too:
/// it fetches a root that is not on disk yet when a chain needs it, honours
/// distrusted roots and sees roots added while the app runs. A snapshot of
/// the ROOT store checked by webpki does none of that, so a perfectly valid
/// certificate could be offered for pinning — and a pinned leaf breaks at the
/// server's next renewal. Where no platform verifier can be built, the OS
/// trust store goes through webpki instead.
fn os_verifier() -> Result<Option<Arc<dyn ServerCertVerifier>>, String> {
    static OS: OnceLock<Result<Option<Arc<dyn ServerCertVerifier>>, String>> = OnceLock::new();
    OS.get_or_init(
        || match rustls_platform_verifier::Verifier::new(provider()) {
            Ok(verifier) => Ok(Some(Arc::new(verifier) as Arc<dyn ServerCertVerifier>)),
            Err(e) => {
                tracing::warn!("no platform certificate verifier ({e}), using the OS trust store");
                webpki_verifier(root_store(), provider())
            }
        },
    )
    .clone()
}

fn client_config(trusted: &[String]) -> AppResult<(ClientConfig, ObservedFingerprint)> {
    client_config_with(trusted, os_verifier().map_err(AppError::Other)?)
}

fn client_config_with(
    trusted: &[String],
    inner: Option<Arc<dyn ServerCertVerifier>>,
) -> AppResult<(ClientConfig, ObservedFingerprint)> {
    let provider = provider();
    let observed: ObservedFingerprint = Arc::new(Mutex::new(None));
    let verifier = PinningVerifier {
        inner,
        provider: provider.clone(),
        trusted: trusted.iter().map(|t| normalize(t)).collect(),
        observed: observed.clone(),
    };
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| AppError::Other(format!("tls setup failed: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok((config, observed))
}

/// Install the pinning TLS backend and the proxy policy
/// ([`crate::system_proxy`]) on a reqwest builder. Returns the builder and the
/// shared cell that will hold the fingerprint the next handshake presents.
pub fn apply(
    builder: reqwest::ClientBuilder,
    trusted: &[String],
) -> AppResult<(reqwest::ClientBuilder, ObservedFingerprint)> {
    let (config, observed) = client_config(trusted)?;
    let builder = crate::system_proxy::apply(builder.tls_backend_preconfigured(config));
    Ok((builder, observed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_canonical_colon_hex() {
        // SHA-256 of the empty input is a known vector.
        let fp = fingerprint(b"");
        assert_eq!(
            fp,
            "E3:B0:C4:42:98:FC:1C:14:9A:FB:F4:C8:99:6F:B9:24:27:AE:41:E4:64:9B:93:4C:A4:95:99:1B:78:52:B8:55"
        );
    }

    #[test]
    fn a_client_builds_without_os_trust_anchors() {
        // A system without a CA bundle must still reach plain-HTTP and pinned
        // servers; webpki alone refuses to build over an empty store.
        let inner = webpki_verifier(Arc::new(RootCertStore::empty()), provider());
        assert!(matches!(inner, Ok(None)));
        let pinned = [fingerprint(b"pinned")];
        assert!(client_config_with(&pinned, None).is_ok());
        assert!(client_config_with(&[], None).is_ok());
    }

    #[test]
    fn public_certificates_are_checked_by_the_os_verifier() {
        let inner = os_verifier().expect("the OS verifier builds");
        // The platform verifier exists on every system the app ships for;
        // only a Unix without any CA bundle is left with pinned leaves alone.
        #[cfg(windows)]
        assert!(inner.is_some());
        assert!(client_config_with(&[], inner).is_ok());
    }

    /// An OS verifier with a fixed answer.
    #[derive(Debug)]
    struct Fixed(bool);

    impl ServerCertVerifier for Fixed {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, TlsError> {
            if self.0 {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(TlsError::InvalidCertificate(
                    CertificateError::UnknownIssuer,
                ))
            }
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, TlsError> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, TlsError> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    fn pinning(inner: Arc<dyn ServerCertVerifier>, trusted: Vec<String>) -> PinningVerifier {
        PinningVerifier {
            inner: Some(inner),
            provider: provider(),
            trusted,
            observed: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn a_publicly_valid_certificate_leaves_nothing_to_confirm() {
        let leaf = CertificateDer::from(b"publicly valid".to_vec());
        let verifier = pinning(Arc::new(Fixed(true)), Vec::new());
        // A fingerprint left over from an earlier failed handshake must not
        // turn the next unrelated network error into a trust prompt.
        *verifier.observed.lock().unwrap() = Some(fingerprint(b"earlier"));
        let name = ServerName::try_from("music.example.com").unwrap();
        assert!(verifier
            .verify_server_cert(&leaf, &[], &name, &[], UnixTime::now())
            .is_ok());
        assert_eq!(*verifier.observed.lock().unwrap(), None);
    }

    #[test]
    fn a_rejected_certificate_passes_only_when_pinned() {
        let leaf = CertificateDer::from(b"self-signed".to_vec());
        let name = ServerName::try_from("music.home").unwrap();
        let now = UnixTime::now();

        let unpinned = pinning(Arc::new(Fixed(false)), Vec::new());
        assert!(matches!(
            unpinned.verify_server_cert(&leaf, &[], &name, &[], now),
            Err(TlsError::InvalidCertificate(
                CertificateError::UnknownIssuer
            ))
        ));
        assert_eq!(
            unpinned.observed.lock().unwrap().as_deref(),
            Some(fingerprint(leaf.as_ref()).as_str()),
            "the fingerprint is offered for confirmation"
        );

        let other = pinning(
            Arc::new(Fixed(false)),
            vec![normalize(&fingerprint(b"other"))],
        );
        assert!(other
            .verify_server_cert(&leaf, &[], &name, &[], now)
            .is_err());

        let pinned = pinning(
            Arc::new(Fixed(false)),
            vec![normalize(&fingerprint(leaf.as_ref()))],
        );
        assert!(pinned
            .verify_server_cert(&leaf, &[], &name, &[], now)
            .is_ok());
    }

    #[test]
    fn the_os_verifier_rejects_what_it_cannot_validate_and_a_pin_still_passes() {
        let Some(os) = os_verifier().expect("the OS verifier builds") else {
            return;
        };
        let leaf = CertificateDer::from(b"not really a certificate".to_vec());
        let name = ServerName::try_from("music.home").unwrap();
        let now = UnixTime::now();
        assert!(pinning(os.clone(), Vec::new())
            .verify_server_cert(&leaf, &[], &name, &[], now)
            .is_err());
        assert!(pinning(os, vec![normalize(&fingerprint(leaf.as_ref()))])
            .verify_server_cert(&leaf, &[], &name, &[], now)
            .is_ok());
    }

    #[test]
    fn without_trust_anchors_only_a_pinned_leaf_passes() {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let leaf = CertificateDer::from(b"not really a certificate".to_vec());
        let verifier = |trusted: Vec<String>| PinningVerifier {
            inner: None,
            provider: provider.clone(),
            trusted,
            observed: Arc::new(Mutex::new(None)),
        };
        let name = ServerName::try_from("music.home").unwrap();
        let now = UnixTime::now();

        let unpinned = verifier(Vec::new());
        assert!(unpinned
            .verify_server_cert(&leaf, &[], &name, &[], now)
            .is_err());
        assert_eq!(
            unpinned.observed.lock().unwrap().as_deref(),
            Some(fingerprint(leaf.as_ref()).as_str()),
            "the fingerprint is offered for confirmation"
        );

        let pinned = verifier(vec![normalize(&fingerprint(leaf.as_ref()))]);
        assert!(pinned
            .verify_server_cert(&leaf, &[], &name, &[], now)
            .is_ok());
        assert!(!pinned.supported_verify_schemes().is_empty());
    }

    #[test]
    fn normalize_ignores_separators_and_case() {
        assert!(matches("ab:cd:ef", "ABCDEF"));
        assert!(matches("AB CD-EF", "ab:cd:ef"));
        assert!(!matches("ab:cd", "ab:ce"));
    }

    #[test]
    fn only_complete_sha256_fingerprints_are_valid() {
        let canonical = fingerprint(b"");
        assert!(is_valid_fingerprint(&canonical));
        assert!(is_valid_fingerprint(&normalize(&canonical).to_lowercase()));
        assert!(is_valid_fingerprint(&canonical.replace(':', " ")));
        // One digit short, one too many, and SHA-1 length.
        assert!(!is_valid_fingerprint(&canonical[..canonical.len() - 1]));
        assert!(!is_valid_fingerprint(&format!("{canonical}:00")));
        assert!(!is_valid_fingerprint(&"AB:".repeat(19)));
        // 64 hex digits hidden in other text must not pass as a fingerprint.
        assert!(!is_valid_fingerprint(&format!("sha256={canonical}")));
        assert!(!is_valid_fingerprint(""));
    }
}
