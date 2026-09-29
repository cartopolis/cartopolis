//! str0m's crypto provider, pure Rust all the way down.
//!
//! A copy of `str0m-rust-crypto` 0.6.0 (MIT OR Apache-2.0, by the str0m
//! authors) — `dtls.rs`, `sha1.rs`, `sha256.rs` and `srtp.rs` are theirs with
//! only the import paths changed — with one difference, which is the reason it
//! is here: the DTLS certificate. Upstream makes it with `dimpl`'s `rcgen`
//! feature, and that feature switches on `aws-lc-rs`, i.e. a C crypto library
//! built by `cc` — found 2026-09-29 when the Android check of this crate began
//! compiling aws-lc. Everything else in upstream's provider is RustCrypto.
//!
//! So [`certificate`] makes the same certificate — self-signed ECDSA P-256, a
//! random 128-bit serial, a year's validity — with RustCrypto's `x509-cert`
//! and `p256`, which `dimpl` already depends on. Drop this module for the
//! upstream crate once it can generate a certificate without aws-lc.

mod dtls;
mod sha1;
mod sha256;
mod srtp;

use str0m::crypto::CryptoProvider;

use dtls::RustCryptoDtlsProvider;
use sha1::RustCryptoSha1HmacProvider;
use sha256::RustCryptoSha256Provider;
use srtp::RustCryptoSrtpProvider;

/// The provider [`crate::install_crypto`] installs.
pub fn provider() -> CryptoProvider {
    static SRTP: RustCryptoSrtpProvider = RustCryptoSrtpProvider;
    static SHA1_HMAC: RustCryptoSha1HmacProvider = RustCryptoSha1HmacProvider;
    static SHA256: RustCryptoSha256Provider = RustCryptoSha256Provider;
    static DTLS: RustCryptoDtlsProvider = RustCryptoDtlsProvider;

    CryptoProvider {
        srtp_provider: &SRTP,
        sha1_hmac_provider: &SHA1_HMAC,
        sha256_provider: &SHA256,
        dtls_provider: &DTLS,
    }
}

/// A self-signed DTLS certificate and its PKCS#8 private key, both DER — the
/// shape `dimpl`'s own generator returns and its signer reads back.
///
/// Nothing verifies the chain: WebRTC authenticates a DTLS peer by the
/// fingerprint the SDP carries, so the certificate only has to be well-formed.
/// The serial is random because Firefox refuses two certificates with one
/// serial from one issuer name, across processes and machines (str0m #517).
pub(crate) fn certificate() -> Option<(Vec<u8>, Vec<u8>)> {
    use p256::ecdsa::{DerSignature, SigningKey};
    use p256::pkcs8::EncodePrivateKey;
    use rand_core::{OsRng, RngCore};
    use std::str::FromStr;
    use std::time::Duration;
    use x509_cert::builder::{Builder, CertificateBuilder, Profile};
    use x509_cert::der::Encode;
    use x509_cert::name::Name;
    use x509_cert::serial_number::SerialNumber;
    use x509_cert::spki::SubjectPublicKeyInfoOwned;
    use x509_cert::time::Validity;

    let key = SigningKey::random(&mut OsRng);
    let mut serial = [0u8; 16];
    OsRng.fill_bytes(&mut serial);
    // DER integers are signed; a clear top bit keeps the serial positive.
    serial[0] = (serial[0] & 0x7f) | 0x01;
    let name = Name::from_str("CN=DTLS Peer,O=DTLS").ok()?;
    let spki = SubjectPublicKeyInfoOwned::from_key(*key.verifying_key()).ok()?;
    let cert = CertificateBuilder::new(
        Profile::Leaf {
            issuer: name.clone(),
            enable_key_agreement: false,
            enable_key_encipherment: false,
        },
        SerialNumber::new(&serial).ok()?,
        Validity::from_now(Duration::from_secs(365 * 24 * 3600)).ok()?,
        name,
        spki,
        &key,
    )
    .ok()?
    .build::<DerSignature>()
    .ok()?;
    let cert_der = cert.to_der().ok()?;
    let key_der = key.to_pkcs8_der().ok()?.as_bytes().to_vec();
    Some((cert_der, key_der))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_certificate_is_distinct_every_time_and_dimpl_accepts_it() {
        let (a, key) = super::certificate().unwrap();
        let (b, _) = super::certificate().unwrap();
        assert_ne!(a, b);
        // dimpl parses the key when it builds a DTLS endpoint; one that it
        // could not read would fail here rather than at a call's handshake.
        let config = std::sync::Arc::new(dimpl::Config::builder().build().unwrap());
        let cert = dimpl::DtlsCertificate {
            certificate: a,
            private_key: key,
        };
        let _ = dimpl::Dtls::new_12(config, cert, std::time::Instant::now());
    }
}
