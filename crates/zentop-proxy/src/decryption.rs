//! Exact-host decryption policy and short-lived TLS identities.
use std::collections::{BTreeSet, VecDeque};
use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use rcgen::{
    CertificateParams, ExtendedKeyUsagePurpose, Issuer, KeyPair, KeyUsagePurpose, SigningKey,
};
use rustls_platform_verifier::BuilderVerifierExt;
use time::{Duration, OffsetDateTime};
use tokio_rustls::rustls::{
    self,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer},
    ClientConfig, ServerConfig,
};

const ALPN: [&[u8]; 2] = [b"h2", b"http/1.1"];
const LEAF_CAPACITY: usize = 256;

pub fn canonical_host(raw: &str) -> Result<String, &'static str> {
    let raw = raw
        .strip_suffix(['.', '\u{3002}', '\u{ff0e}', '\u{ff61}'])
        .unwrap_or(raw);
    let host = idna::domain_to_ascii_strict(raw).map_err(|_| "Enter an exact DNS hostname")?;
    if host.is_empty()
        || host.len() > 253
        || host.parse::<IpAddr>().is_ok()
        || host.split('.').any(|label| {
            label.is_empty()
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err("Enter an exact DNS hostname");
    }
    Ok(host.to_ascii_lowercase())
}

pub fn skip_reason(host: &str) -> Option<&'static str> {
    const APPLE: &[&str] = &[
        "apple.com",
        "icloud.com",
        "icloud-content.com",
        "apple-cloudkit.com",
        "mzstatic.com",
        "itunes.com",
        "me.com",
        "mac.com",
        "aaplimg.com",
        "apple-dns.net",
        "cdn-apple.com",
    ];
    const PINNED: &[&str] = &[
        "accounts.google.com",
        "signal.org",
        "whatsapp.com",
        "whatsapp.net",
    ];
    let belongs = |domain: &&str| {
        host == *domain
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    };
    if APPLE.iter().any(belongs) {
        Some("Apple services are on the built-in skip list")
    } else if PINNED.iter().any(belongs) {
        Some("This service is on the built-in certificate pinning skip list")
    } else {
        None
    }
}

pub fn decrypt_host(raw: &str) -> Result<String, &'static str> {
    let host = canonical_host(raw.trim())?;
    if let Some(reason) = skip_reason(&host) {
        return Err(reason);
    }
    Ok(host)
}

/// Base64 of the SHA-256 of a DER SubjectPublicKeyInfo, the form Chromium's
/// `--ignore-certificate-errors-spki-list` takes.
pub fn spki_sha256(spki: &[u8]) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(spki))
}

/// One in-memory key shared by every leaf in a capture session, created
/// only for apps launched with Chromium's SPKI switch. Chromium accepts any
/// presented certificate whose key hash is listed, so the listed key must
/// never appear in a public certificate, such as the exported root.
struct SessionKey {
    key: KeyPair,
    spki: String,
}

pub(crate) fn server_config(
    issuer: &Issuer<'_, impl SigningKey>,
    host: &str,
    expires: OffsetDateTime,
    session_key: Option<&KeyPair>,
) -> io::Result<ServerConfig> {
    let generated;
    let key = match session_key {
        Some(key) => key,
        None => {
            generated =
                KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(io::Error::other)?;
            &generated
        }
    };
    let mut params = CertificateParams::new(vec![host.to_owned()]).map_err(io::Error::other)?;
    params.not_before = OffsetDateTime::now_utc() - Duration::minutes(5);
    params.not_after = expires;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params.signed_by(key, issuer).map_err(io::Error::other)?;
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .map_err(io::Error::other)?;
    config.alpn_protocols = ALPN.iter().map(|protocol| protocol.to_vec()).collect();
    Ok(config)
}

type SignLeaf =
    dyn Fn(&str, OffsetDateTime, Option<&KeyPair>) -> io::Result<ServerConfig> + Send + Sync;
type CheckKey = dyn Fn() -> io::Result<()> + Send + Sync;

#[derive(Clone)]
pub struct CertificateAuthority {
    sign: Arc<SignLeaf>,
    expires: OffsetDateTime,
    check: Arc<CheckKey>,
}

impl CertificateAuthority {
    fn check(&self) -> io::Result<()> {
        if self.expires <= OffsetDateTime::now_utc() {
            return Err(io::Error::other("The root certificate has expired"));
        }
        (self.check)()
    }

    /// Creates the leaf issuer from a certificate and its signing interface.
    pub fn new(
        certificate: &CertificateDer<'_>,
        key: impl SigningKey + Send + Sync + 'static,
        expires: OffsetDateTime,
    ) -> io::Result<Self> {
        let issuer = Issuer::from_ca_cert_der(certificate, key).map_err(io::Error::other)?;
        Ok(Self {
            sign: Arc::new(move |host, expires, key| server_config(&issuer, host, expires, key)),
            expires,
            check: Arc::new(|| Ok(())),
        })
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn with_key_check(
        mut self,
        check: impl Fn() -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.check = Arc::new(check);
        self
    }
}

struct Leaf {
    host: String,
    expires: OffsetDateTime,
    config: Arc<ServerConfig>,
}

#[derive(Default)]
struct State {
    hosts: BTreeSet<String>,
    authority: Option<CertificateAuthority>,
    leaves: VecDeque<Leaf>,
    error: Option<String>,
}

impl State {
    fn disable(&mut self, authority: &CertificateAuthority, error: &io::Error) {
        if self
            .authority
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.sign, &authority.sign))
        {
            self.leaves.clear();
            self.error = Some(format!("Certificate key unavailable: {error}"));
        }
    }
}

/// GUI-owned policy shared with the running session.
pub struct Decryption {
    state: Mutex<State>,
    signing: Mutex<()>,
    session_key: Mutex<Option<Arc<SessionKey>>>,
    pub(crate) upstream: Arc<ClientConfig>,
}

impl Decryption {
    pub fn platform() -> io::Result<Self> {
        let config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(io::Error::other)?
                .with_platform_verifier()
                .map_err(io::Error::other)?
                .with_no_client_auth();
        Ok(Self::with_client_config(config))
    }

    /// Allows an isolated test to trust its own upstream test CA.
    pub fn with_client_config(mut upstream: ClientConfig) -> Self {
        upstream.alpn_protocols = ALPN.iter().map(|protocol| protocol.to_vec()).collect();
        Self {
            state: Mutex::default(),
            signing: Mutex::default(),
            session_key: Mutex::default(),
            upstream: Arc::new(upstream),
        }
    }

    /// Switches this session's leaves to one shared in-memory key and returns
    /// that key's SPKI hash for `--ignore-certificate-errors-spki-list`. Calls
    /// after the first return the same hash. Cached per-host leaves are
    /// dropped, so every leaf issued from now on carries the shared key.
    pub fn session_key_spki(&self) -> io::Result<String> {
        let mut session_key = self.session_key.lock().unwrap();
        if let Some(existing) = session_key.as_ref() {
            return Ok(existing.spki.clone());
        }
        let key =
            KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(io::Error::other)?;
        let spki = spki_sha256(&rcgen::PublicKeyData::subject_public_key_info(&key));
        *session_key = Some(Arc::new(SessionKey {
            key,
            spki: spki.clone(),
        }));
        drop(session_key);
        self.state.lock().unwrap().leaves.clear();
        Ok(spki)
    }

    pub fn set_hosts(&self, hosts: impl IntoIterator<Item = String>) {
        self.state.lock().unwrap().hosts = hosts
            .into_iter()
            .filter_map(|host| decrypt_host(&host).ok())
            .collect();
    }

    pub fn set_authority(&self, authority: Option<CertificateAuthority>) {
        let mut state = self.state.lock().unwrap();
        state.authority = authority;
        state.leaves.clear();
    }

    pub fn disable_for_session(&self, reason: String) {
        let mut state = self.state.lock().unwrap();
        state.leaves.clear();
        state.error.get_or_insert(reason);
    }

    pub fn error(&self) -> Option<String> {
        self.state.lock().unwrap().error.clone()
    }

    fn authority(&self, host: &str) -> Option<CertificateAuthority> {
        let state = self.state.lock().unwrap();
        state.authority.clone().filter(|_| {
            state.error.is_none() && state.hosts.contains(host) && skip_reason(host).is_none()
        })
    }

    pub(crate) fn selected(&self, host: &str) -> bool {
        self.authority(host).is_some()
    }

    pub(crate) fn available(&self, host: &str) -> bool {
        let Some(authority) = self.authority(host) else {
            return false;
        };
        if let Err(error) = authority.check() {
            let mut state = self.state.lock().unwrap();
            state.disable(&authority, &error);
            return false;
        }
        true
    }

    /// A still-valid cached leaf for `host`, if decryption is currently allowed for it.
    fn cached_leaf(&self, host: &str) -> Option<Arc<ServerConfig>> {
        let state = self.state.lock().unwrap();
        if state.error.is_some()
            || state.authority.is_none()
            || !state.hosts.contains(host)
            || skip_reason(host).is_some()
        {
            return None;
        }
        let now = OffsetDateTime::now_utc();
        state
            .leaves
            .iter()
            .find(|entry| entry.host == host && entry.expires > now)
            .map(|entry| entry.config.clone())
    }

    pub(crate) fn leaf(&self, host: &str) -> io::Result<Option<Arc<ServerConfig>>> {
        // A cached leaf needs no Keychain signature, so it must not wait behind
        // another host's signing, which can block on an OS prompt.
        if let Some(config) = self.cached_leaf(host) {
            return Ok(Some(config));
        }
        // Keychain access can block on an OS prompt. Keep the engine and GUI
        // free to read or change policy while this worker waits.
        let _signing = self.signing.lock().unwrap();
        let Some(authority) = self.authority(host) else {
            return Ok(None);
        };
        let now = OffsetDateTime::now_utc();
        let expires = (now + Duration::days(7)).min(authority.expires);
        let session_key = self.session_key.lock().unwrap().clone();
        let result = (|| {
            authority.check()?;
            let mut state = self.state.lock().unwrap();
            let cached = state
                .leaves
                .iter()
                .position(|entry| entry.host == host)
                .and_then(|index| state.leaves.remove(index))
                .filter(|entry| entry.expires > now);
            if let Some(entry) = cached {
                return Ok((entry.config, entry.expires));
            }
            drop(state);
            (authority.sign)(
                host,
                expires,
                session_key.as_ref().map(|shared| &shared.key),
            )
            .map(|config| (Arc::new(config), expires))
        })();
        let key_changed = {
            let current = self.session_key.lock().unwrap();
            match (current.as_ref(), session_key.as_ref()) {
                (Some(current), Some(used)) => !Arc::ptr_eq(current, used),
                (current, used) => current.is_some() != used.is_some(),
            }
        };
        let mut state = self.state.lock().unwrap();
        if state.error.is_some()
            || !state.hosts.contains(host)
            || state
                .authority
                .as_ref()
                .is_none_or(|current| !Arc::ptr_eq(&current.sign, &authority.sign))
        {
            return Ok(None);
        }
        match result {
            // The session key was switched on while this leaf was signed;
            // serve it once, but don't cache a leaf with the old key.
            Ok((config, _)) if key_changed => Ok(Some(config)),
            Ok((config, expires)) => {
                state.leaves.retain(|entry| entry.host != host);
                state.leaves.push_front(Leaf {
                    host: host.into(),
                    expires,
                    config: config.clone(),
                });
                state.leaves.truncate(LEAF_CAPACITY);
                Ok(Some(config))
            }
            Err(error) => {
                state.disable(&authority, &error);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_only_listed_hosts_with_a_usable_root() {
        let mut params = CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        let key = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let authority = CertificateAuthority::new(
            cert.der(),
            key,
            OffsetDateTime::now_utc() + Duration::days(365),
        )
        .unwrap();
        let tls = Decryption::platform().unwrap();
        tls.set_hosts(["example.com".into(), "www.apple.com".into()]);
        assert!(!tls.selected("example.com"));
        tls.set_authority(Some(authority.clone()));
        assert!(tls.selected("example.com"));
        assert!(!tls.selected("api.example.com"));
        assert!(!tls.selected("www.apple.com"));
        tls.disable_for_session("Keychain is locked".into());
        tls.set_authority(Some(authority));
        assert!(!tls.selected("example.com"));
        assert!(tls.leaf("example.com").unwrap().is_none());
    }

    #[test]
    fn canonicalizes_exact_dns_names_and_excludes_skip_domains() {
        assert_eq!(
            decrypt_host(" BüCHER.Example. "),
            Ok("xn--bcher-kva.example".into())
        );
        for host in [
            "*.example.com",
            "apple.com.",
            "www.APPLE.COM",
            "example.com:443",
            "example.com..",
            "127.0.0.1",
            "https://example.com",
        ] {
            assert!(decrypt_host(host).is_err(), "{host}");
        }
        assert_eq!(decrypt_host("notapple.com"), Ok("notapple.com".into()));
    }
}
