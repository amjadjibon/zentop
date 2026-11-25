//! macOS root identity, user trust, and recovery from interrupted changes.
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use core_foundation::{
    array::{CFArray, CFArrayRef},
    base::{CFType, TCFType},
    boolean::CFBoolean,
    dictionary::{CFDictionary, CFDictionaryRef},
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use rcgen::{
    CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyUsagePurpose, PublicKeyData,
    SigningKey,
};
use security_framework::{
    certificate::SecCertificate,
    item::{
        AddRef, ItemAddOptions, ItemAddValue, ItemClass, ItemSearchOptions, KeyClass, Limit,
        Location, Reference, SearchResult,
    },
    key::{Algorithm, SecKey},
    os::macos::keychain::SecKeychain,
    policy::SecPolicy,
};
use security_framework_sys::{
    base::{errSecItemNotFound, errSecNoTrustSettings, SecKeychainRef, SecPolicyRef},
    item::{
        kSecAttrIsPermanent, kSecAttrKeySizeInBits, kSecAttrKeyType,
        kSecAttrKeyTypeECSECPrimeRandom, kSecAttrLabel, kSecPrivateKeyAttrs, kSecUseKeychain,
    },
    keychain_item::SecKeychainItemDelete,
    trust_settings::{kSecTrustSettingsDomainUser, SecTrustSettingsCopyTrustSettings},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use tokio_rustls::rustls::pki_types::CertificateDer;

use crate::CertificateAuthority;

const KEY_PREFIX: &str = "Zentop Proxy Root ";
const SIGNATURE: Algorithm = Algorithm::ECDSASignatureMessageX962SHA256;

unsafe extern "C" {
    static kSecAttrIsExtractable: CFStringRef;
    static kSecAttrIsSensitive: CFStringRef;
    fn SecKeychainGetStatus(keychain: SecKeychainRef, status: *mut u32) -> i32;
    fn SecPolicyCopyProperties(policy: SecPolicyRef) -> CFDictionaryRef;
    static kSecPolicyOid: CFStringRef;
    static kSecPolicyAppleSSL: CFStringRef;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertificateState {
    None,
    Created,
    Trusted,
    KeyUnavailable,
    Expiring,
    PartlyRemoved,
}

#[derive(Clone)]
pub struct CertificateStatus {
    pub state: CertificateState,
    pub fingerprint: Option<String>,
    pub expires: Option<String>,
    pub explanation: Vec<String>,
    pub pem: Option<String>,
    pub trust_available: bool,
}

#[derive(Clone, Copy)]
pub enum CertificateAction {
    Refresh,
    Create,
    Trust,
    Remove,
    Replace,
    RetryKey,
}

#[derive(Default, Serialize, Deserialize)]
struct Journal {
    active: Option<Root>,
    pending: Option<Root>,
    retired: Option<Root>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Root {
    key_label: String,
    der: Option<Vec<u8>>,
    expires: i64,
    removing: bool,
}

impl Root {
    fn fingerprint(&self) -> Option<String> {
        self.der
            .as_ref()
            .map(|der| format!("{:X}", Sha256::digest(der)))
    }

    fn pem(&self) -> Option<String> {
        let encoded = STANDARD.encode(self.der.as_ref()?);
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for line in encoded.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(line).ok()?);
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        Some(pem)
    }
}

struct KeychainKey {
    key: SecKey,
    public: Vec<u8>,
}

impl KeychainKey {
    fn new(key: SecKey) -> io::Result<Self> {
        let public = key
            .public_key()
            .and_then(|key| key.external_representation())
            .ok_or_else(|| io::Error::other("Could not read the root's public key"))?;
        Ok(Self {
            key,
            public: public.to_vec(),
        })
    }
}

impl PublicKeyData for KeychainKey {
    fn der_bytes(&self) -> &[u8] {
        &self.public
    }

    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

impl SigningKey for KeychainKey {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        self.key
            .create_signature(SIGNATURE, message)
            .map_err(|_| rcgen::Error::RemoteKeyError)
    }
}

trait Trust: Send + Sync {
    fn available(&self) -> bool;
    fn contains(&self, root: &Root) -> io::Result<bool>;
    fn trusts_ssl(&self, root: &Root) -> io::Result<bool> {
        self.contains(root)
    }
    fn change(&self, root: &Root, keychain: &Path, add: bool) -> io::Result<()>;
}

struct UserTrust {
    isolated: bool,
}

impl UserTrust {
    fn settings(&self, root: &Root) -> io::Result<Option<CFArray<CFType>>> {
        let Some(der) = root.der.as_deref().filter(|_| self.available()) else {
            return Ok(None);
        };
        let cert = SecCertificate::from_der(der).map_err(io::Error::other)?;
        let mut settings: CFArrayRef = std::ptr::null();
        // The certificate is owned here and the result follows the create rule.
        let status = unsafe {
            SecTrustSettingsCopyTrustSettings(
                cert.as_concrete_TypeRef(),
                kSecTrustSettingsDomainUser,
                &mut settings,
            )
        };
        match status {
            0 if !settings.is_null() => {
                Ok(Some(unsafe { CFArray::wrap_under_create_rule(settings) }))
            }
            status if status == errSecItemNotFound || status == errSecNoTrustSettings => Ok(None),
            status => Err(os_error(status)),
        }
    }
}

impl Trust for UserTrust {
    fn available(&self) -> bool {
        !self.isolated
    }

    fn contains(&self, root: &Root) -> io::Result<bool> {
        Ok(self.settings(root)?.is_some())
    }

    fn trusts_ssl(&self, root: &Root) -> io::Result<bool> {
        let Some(settings) = self.settings(root)? else {
            return Ok(false);
        };
        if settings.is_empty() {
            return Ok(true); // macOS treats an explicitly empty array as unrestricted root trust.
        }
        let attribute = |entry: &CFDictionary, name: &str| {
            entry
                .find(CFString::new(name).as_CFTypeRef().cast())
                .map(|value| unsafe { CFType::wrap_under_get_rule((*value).cast()) })
        };
        let mut trusted = false;
        for entry in settings
            .iter()
            .filter_map(|entry| entry.downcast::<CFDictionary>())
        {
            if let Some(value) = attribute(&entry, "kSecTrustSettingsPolicy") {
                let Some(policy) = value.downcast::<SecPolicy>() else {
                    continue;
                };
                let properties = unsafe { SecPolicyCopyProperties(policy.as_concrete_TypeRef()) };
                if properties.is_null() {
                    continue;
                }
                let properties: CFDictionary =
                    unsafe { CFDictionary::wrap_under_create_rule(properties) };
                let Some(oid) = properties.find(unsafe { kSecPolicyOid.cast() }) else {
                    continue;
                };
                let oid = unsafe { CFString::wrap_under_get_rule((*oid).cast()) };
                if oid != unsafe { CFString::wrap_under_get_rule(kSecPolicyAppleSSL) } {
                    continue;
                }
            }
            let result = attribute(&entry, "kSecTrustSettingsResult")
                .and_then(|value| value.downcast::<CFNumber>())
                .and_then(|number| number.to_i32())
                .unwrap_or(1);
            if result == 3 {
                return Ok(false);
            }
            trusted |= result == 1
                && [
                    "kSecTrustSettingsApplication",
                    "kSecTrustSettingsPolicyString",
                    "kSecTrustSettingsKeyUsage",
                ]
                .iter()
                .all(|name| attribute(&entry, name).is_none());
        }
        Ok(trusted)
    }

    fn change(&self, root: &Root, keychain: &Path, add: bool) -> io::Result<()> {
        if !self.available() {
            return Err(io::Error::other(
                "User trust is disabled for the isolated development keychain",
            ));
        }
        let pem = root
            .pem()
            .ok_or_else(|| io::Error::other("The public certificate is missing"))?;
        let mut file = tempfile::Builder::new().suffix(".pem").tempfile()?;
        file.write_all(pem.as_bytes())?;
        let mut command = Command::new("/usr/bin/security");
        if add {
            command
                .args(["add-trusted-cert", "-r", "trustRoot", "-p", "ssl", "-k"])
                .arg(keychain);
        } else {
            command.arg("remove-trusted-cert");
        }
        let output = command.arg(file.path()).output()?;
        if output.status.success() {
            Ok(())
        } else {
            let error = String::from_utf8_lossy(&output.stderr);
            Err(io::Error::other(format!(
                "macOS could not change user trust: {}",
                error.trim()
            )))
        }
    }
}

/// Holds public recovery data only. Every keychain query is scoped to one file.
pub struct Certificates {
    path: PathBuf,
    keychain_path: PathBuf,
    trust: Box<dyn Trust>,
    journal: Journal,
    key_error: Option<String>,
}

impl Certificates {
    pub fn open(config_directory: &Path) -> io::Result<Self> {
        let isolated = std::env::var_os("ZENTOP_KEYCHAIN").map(PathBuf::from);
        if isolated.is_some() && !cfg!(debug_assertions) {
            return Err(io::Error::other(
                "ZENTOP_KEYCHAIN is available in development builds",
            ));
        }
        let path = match &isolated {
            Some(path) if path.is_absolute() => path.clone(),
            Some(_) => return Err(io::Error::other("ZENTOP_KEYCHAIN must be an absolute path")),
            None => std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join("Library/Keychains/login.keychain-db"))
                .ok_or_else(|| io::Error::other("Could not locate the login keychain"))?,
        };
        let journal_name = if isolated.is_some() {
            let hash = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
            format!("traffic-ca-dev-{}.json", &hash[..16])
        } else {
            "traffic-ca.json".into()
        };
        Self::open_with(
            config_directory.join(journal_name),
            path,
            Box::new(UserTrust {
                isolated: isolated.is_some(),
            }),
        )
    }

    fn open_with(path: PathBuf, keychain_path: PathBuf, trust: Box<dyn Trust>) -> io::Result<Self> {
        let journal = Self::read_journal(&path)?;
        Ok(Self {
            path,
            keychain_path,
            trust,
            journal,
            key_error: None,
        })
    }

    fn read_journal(path: &Path) -> io::Result<Journal> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Journal::default()),
            Err(error) => Err(error),
        }
    }

    fn persist(&self) -> io::Result<()> {
        let directory = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("Configuration directory unavailable"))?;
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(&mut file, &self.journal)?;
        file.as_file().sync_all()?;
        file.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }

    pub fn perform(&mut self, action: CertificateAction) -> io::Result<()> {
        if matches!(action, CertificateAction::Refresh) {
            self.journal = Self::read_journal(&self.path)?;
            return Ok(());
        }
        let directory = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("Configuration directory unavailable"))?;
        fs::create_dir_all(directory)?;
        let lock = fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.with_extension("lock"))?;
        lock.try_lock()
            .map_err(|error| io::Error::other(format!("Certificate change is busy: {error}")))?;
        self.journal = Self::read_journal(&self.path)?;
        match action {
            CertificateAction::Refresh => Ok(()),
            CertificateAction::Create
                if self.journal.active.is_none() && self.journal.pending.is_none() =>
            {
                self.create(false)
            }
            CertificateAction::Trust => {
                let root = self
                    .journal
                    .active
                    .as_ref()
                    .filter(|root| !root.removing)
                    .ok_or_else(|| io::Error::other("Create a certificate first"))?;
                self.change_trust(root, true)
            }
            CertificateAction::RetryKey => {
                let mut keychain = self.keychain()?;
                let result = (|| {
                    if keychain_unlocked(&keychain).is_err() {
                        keychain.unlock(None).map_err(io::Error::other)?;
                    }
                    let root = self
                        .journal
                        .active
                        .as_ref()
                        .ok_or_else(|| io::Error::other("No root key"))?;
                    let key = self
                        .find_key(&keychain, root)?
                        .ok_or_else(|| io::Error::other("Root key missing"))?;
                    key.create_signature(SIGNATURE, b"Zentop root key availability")
                        .map_err(|error| io::Error::other(error.to_string()))?;
                    Ok::<_, io::Error>(())
                })();
                self.key_error = result.as_ref().err().map(ToString::to_string);
                result
            }
            CertificateAction::Replace => {
                if !self.trust.available() {
                    return Err(io::Error::other(
                        "User trust is disabled for the isolated development keychain",
                    ));
                }
                if self.journal.pending.is_some() || self.journal.retired.is_some() {
                    return Err(io::Error::other(
                        "Finish the previous certificate change first",
                    ));
                }
                self.create(true)
            }
            CertificateAction::Remove => self.remove(),
            CertificateAction::Create => Err(io::Error::other("A certificate already exists")),
        }
    }

    fn change_trust(&self, root: &Root, add: bool) -> io::Result<()> {
        // Verify independently, including if the tool reports an error after
        // completing the trust change.
        let change = self.trust.change(root, &self.keychain_path, add);
        let applied = if add {
            self.trust.trusts_ssl(root)?
        } else {
            !self.trust.contains(root)?
        };
        if applied {
            return Ok(());
        }
        change?;
        Err(io::Error::other(
            "macOS did not apply the requested trust change",
        ))
    }

    fn keychain(&self) -> io::Result<SecKeychain> {
        SecKeychain::open(&self.keychain_path).map_err(io::Error::other)
    }

    fn create(&mut self, replace: bool) -> io::Result<()> {
        let keychain = self.keychain()?;
        keychain_unlocked(&keychain)?;
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(io::Error::other)?;
        let key_label = format!(
            "{KEY_PREFIX}{}",
            id.iter().fold(String::new(), |mut out, byte| {
                use std::fmt::Write as _;
                let _ = write!(out, "{byte:02x}");
                out
            })
        );
        let expires = OffsetDateTime::now_utc() + Duration::days(365);
        self.journal.pending = Some(Root {
            key_label,
            der: None,
            expires: expires.unix_timestamp(),
            removing: false,
        });
        self.persist()?;
        let result = self.create_pending(&keychain, expires, replace);
        if result.is_err() {
            // If trust or signing was cancelled, remove the candidate and keep
            // the previously active identity. Keep the recovery record if cleanup fails.
            let _ = self.remove_pending();
        }
        result
    }

    fn create_pending(
        &mut self,
        keychain: &SecKeychain,
        expires: OffsetDateTime,
        replace: bool,
    ) -> io::Result<()> {
        let pending = self
            .journal
            .pending
            .as_ref()
            .ok_or_else(|| io::Error::other("No pending certificate"))?;
        let key = generate_key(keychain, &pending.key_label)?;
        let key = KeychainKey::new(key)?;
        let mut params = CertificateParams::default();
        params.distinguished_name.push(
            DnType::CommonName,
            format!("Zentop Local Proxy CA ({})", machine_name()),
        );
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let mut serial = [0u8; 20];
        getrandom::fill(&mut serial).map_err(io::Error::other)?;
        serial[0] &= 0x7f;
        params.serial_number = Some(serial.to_vec().into());
        params.not_before = OffsetDateTime::now_utc() - Duration::minutes(5);
        params.not_after = expires;
        let cert = params.self_signed(&key).map_err(io::Error::other)?;
        let pending = self
            .journal
            .pending
            .as_mut()
            .ok_or_else(|| io::Error::other("No pending certificate"))?;
        pending.der = Some(cert.der().to_vec());
        self.persist()?;
        let pending = self
            .journal
            .pending
            .as_ref()
            .ok_or_else(|| io::Error::other("No pending certificate"))?;
        let cert = SecCertificate::from_der(cert.der()).map_err(io::Error::other)?;
        ItemAddOptions::new(ItemAddValue::Ref(AddRef::Certificate(cert)))
            .set_location(Location::FileKeychain(keychain.clone()))
            .add()
            .map_err(io::Error::other)?;
        if replace {
            self.change_trust(pending, true)?;
        }
        let old = self.journal.active.take();
        self.journal.active = self.journal.pending.take();
        self.journal.retired = old;
        if let Err(error) = self.persist() {
            self.journal.pending = self.journal.active.take();
            self.journal.active = self.journal.retired.take();
            return Err(error);
        }
        self.key_error = None;
        self.remove_retired()
    }

    fn find_key(&self, chain: &SecKeychain, root: &Root) -> io::Result<Option<SecKey>> {
        if !root.key_label.starts_with(KEY_PREFIX) {
            return Err(io::Error::other("Invalid Zentop key identifier"));
        }
        let keys = search(
            ItemSearchOptions::new()
                .keychains(std::slice::from_ref(chain))
                .class(ItemClass::key())
                .key_class(KeyClass::private())
                .label(&root.key_label)
                .load_refs(true),
        )?;
        Ok(keys.into_iter().find_map(|item| match item {
            SearchResult::Ref(Reference::Key(key)) => Some(key),
            _ => None,
        }))
    }

    fn find_certificate(
        &self,
        chain: &SecKeychain,
        root: &Root,
    ) -> io::Result<Option<SecCertificate>> {
        let Some(der) = root.der.as_deref() else {
            return Ok(None);
        };
        let certs = search(
            ItemSearchOptions::new()
                .keychains(std::slice::from_ref(chain))
                .class(ItemClass::certificate())
                .load_refs(true)
                .limit(Limit::All),
        )?;
        Ok(certs.into_iter().find_map(|item| match item {
            SearchResult::Ref(Reference::Certificate(cert)) if cert.to_der() == der => Some(cert),
            _ => None,
        }))
    }

    fn remove_pending(&mut self) -> io::Result<()> {
        if let Some(root) = &self.journal.pending {
            self.remove_parts(root)?;
            self.journal.pending = None;
            self.persist()?;
        }
        Ok(())
    }

    fn remove_retired(&mut self) -> io::Result<()> {
        if let Some(root) = &self.journal.retired {
            self.remove_parts(root)?;
            self.journal.retired = None;
            self.persist()?;
        }
        Ok(())
    }

    fn remove(&mut self) -> io::Result<()> {
        // During recovery, retry only the interrupted candidate or retirement.
        if self.journal.pending.is_some() || self.journal.retired.is_some() {
            self.remove_pending()?;
            return self.remove_retired();
        }
        let Some(root) = self.journal.active.as_ref() else {
            return Ok(());
        };
        let keychain = self.keychain()?;
        keychain_unlocked(&keychain)?;
        if self.trust.contains(root)? {
            self.change_trust(root, false)?;
        }
        if let Some(root) = &mut self.journal.active {
            root.removing = true;
        }
        self.persist()?;
        if let Some(root) = &self.journal.active {
            self.remove_parts(root)?;
        }
        self.journal.active = None;
        self.key_error = None;
        self.persist()
    }

    fn remove_parts(&self, root: &Root) -> io::Result<()> {
        let chain = self.keychain()?;
        keychain_unlocked(&chain)?;
        if self.trust.contains(root)? {
            self.change_trust(root, false)?;
        }
        if let Some(certificate) = self.find_certificate(&chain, root)? {
            // This is the persistent item returned by a search of the selected keychain.
            unsafe { SecKeychainItemDelete(certificate.as_concrete_TypeRef().cast()) };
        }
        if let Some(key) = self.find_key(&chain, root)? {
            let _ = key.delete();
        }
        let trust_left = self.trust.contains(root);
        let cert_left = self.find_certificate(&chain, root);
        let key_left = self.find_key(&chain, root);
        if trust_left? || cert_left?.is_some() || key_left?.is_some() {
            return Err(io::Error::other(
                "Some certificate items remain in Keychain",
            ));
        }
        Ok(())
    }

    pub fn inspect(&self) -> (CertificateStatus, Option<CertificateAuthority>) {
        let mut status = CertificateStatus {
            state: CertificateState::None,
            fingerprint: None,
            expires: None,
            explanation: Vec::new(),
            pem: None,
            trust_available: self.trust.available(),
        };
        let leftover = self
            .journal
            .pending
            .as_ref()
            .or(self.journal.retired.as_ref())
            .or(self.journal.active.as_ref().filter(|root| root.removing));
        if let Some(root) = leftover.or(self.journal.active.as_ref()) {
            status.fingerprint = root.fingerprint();
            status.pem = root.pem();
            status.expires = OffsetDateTime::from_unix_timestamp(root.expires)
                .ok()
                .map(|date| date.date().to_string());
        }
        let authority = self.inspect_active(&mut status).unwrap_or_else(|error| {
            status.state = CertificateState::KeyUnavailable;
            status.explanation.push(error.to_string());
            None
        });
        if let Some(root) = leftover {
            status.state = CertificateState::PartlyRemoved;
            status.explanation.extend(self.leftovers(root));
        }
        (status, authority)
    }

    fn inspect_active(
        &self,
        status: &mut CertificateStatus,
    ) -> io::Result<Option<CertificateAuthority>> {
        let Some(root) = self.journal.active.as_ref().filter(|root| !root.removing) else {
            return Ok(None);
        };
        let chain = self.keychain()?;
        keychain_unlocked(&chain)?;
        if self.find_certificate(&chain, root)?.is_none() {
            status.state = CertificateState::PartlyRemoved;
            status.explanation.extend(self.leftovers(root));
            return Ok(None);
        }
        let key = self
            .find_key(&chain, root)?
            .ok_or_else(|| io::Error::other("The root key is missing from Keychain"))?;
        if let Some(reason) = &self.key_error {
            return Err(io::Error::other(reason.clone()));
        }
        let trusted = self.trust.trusts_ssl(root)?;
        let expires =
            OffsetDateTime::from_unix_timestamp(root.expires).map_err(io::Error::other)?;
        let now = OffsetDateTime::now_utc();
        status.state = if expires <= now + Duration::days(30) {
            CertificateState::Expiring
        } else if trusted {
            CertificateState::Trusted
        } else {
            CertificateState::Created
        };
        if expires <= now {
            status
                .explanation
                .push("The certificate has expired".into());
            return Ok(None);
        }
        let der = root
            .der
            .as_deref()
            .ok_or_else(|| io::Error::other("The root certificate is missing"))?;
        let issuer =
            CertificateAuthority::new(&CertificateDer::from(der), KeychainKey::new(key)?, expires)?
                .with_key_check(move || keychain_unlocked(&chain));
        Ok(Some(issuer))
    }

    fn leftovers(&self, root: &Root) -> Vec<String> {
        let mut lines = Vec::new();
        let chain = self.keychain();
        match self.trust.contains(root) {
            Ok(true) => lines.push(
                "Trust: export this certificate, then run security remove-trusted-cert <PEM path>"
                    .into(),
            ),
            Err(error) => lines.push(format!("Could not verify trust removal: {error}")),
            Ok(false) => {}
        }
        match chain
            .as_ref()
            .map_err(|e| io::Error::other(e.to_string()))
            .and_then(|chain| self.find_certificate(chain, root))
        {
            Ok(Some(_)) => {
                if let Some(hash) = root.fingerprint() {
                    lines.push(format!(
                        "Certificate: security delete-certificate -Z {hash} {}",
                        shell_quote(&self.keychain_path)
                    ));
                }
            }
            Err(error) => lines.push(format!("Could not verify certificate removal: {error}")),
            Ok(None) => {}
        }
        match chain
            .as_ref()
            .map_err(|e| io::Error::other(e.to_string()))
            .and_then(|chain| self.find_key(chain, root))
        {
            Ok(Some(_)) => lines.push(format!(
                "Key: delete “{}” in Keychain Access ({})",
                root.key_label,
                self.keychain_path.display()
            )),
            Err(error) => lines.push(format!("Could not verify key removal: {error}")),
            Ok(None) => {}
        }
        if lines.is_empty() {
            lines.push("Retry removal to clear the interrupted certificate change".into());
        }
        lines
    }
}

fn search(query: &ItemSearchOptions) -> io::Result<Vec<SearchResult>> {
    match query.search() {
        Ok(results) => Ok(results),
        Err(error) if error.code() == errSecItemNotFound => Ok(Vec::new()),
        Err(error) => Err(io::Error::other(error)),
    }
}

fn keychain_unlocked(keychain: &SecKeychain) -> io::Result<()> {
    let mut status = 0;
    let result = unsafe { SecKeychainGetStatus(keychain.as_concrete_TypeRef(), &mut status) };
    if result != 0 {
        return Err(os_error(result));
    }
    if status & 1 == 0 {
        return Err(io::Error::other("The Keychain is locked"));
    }
    Ok(())
}

fn os_error(status: i32) -> io::Error {
    io::Error::other(security_framework::base::Error::from_code(status))
}

fn generate_key(chain: &SecKeychain, label: &str) -> io::Result<SecKey> {
    let attr = |name| unsafe { CFString::wrap_under_get_rule(name) };
    let private: CFDictionary<CFString, CFType> = CFDictionary::from_CFType_pairs(&[
        (
            attr(unsafe { kSecAttrIsPermanent }),
            CFBoolean::true_value().as_CFType(),
        ),
        (
            attr(unsafe { kSecAttrIsSensitive }),
            CFBoolean::true_value().as_CFType(),
        ),
        (
            attr(unsafe { kSecAttrLabel }),
            CFString::new(label).as_CFType(),
        ),
    ]);
    let extractable = attr(unsafe { kSecAttrIsExtractable });
    let attributes = CFDictionary::from_CFType_pairs(&[
        // The macOS file-keychain backend reads extractability at the top level.
        (extractable.clone(), CFBoolean::false_value().as_CFType()),
        (
            attr(unsafe { kSecAttrKeyType }),
            attr(unsafe { kSecAttrKeyTypeECSECPrimeRandom }).as_CFType(),
        ),
        (
            attr(unsafe { kSecAttrKeySizeInBits }),
            CFNumber::from(256).as_CFType(),
        ),
        (attr(unsafe { kSecUseKeychain }), chain.as_CFType()),
        (attr(unsafe { kSecPrivateKeyAttrs }), private.as_CFType()),
    ]);
    // GenerateKeyOptions cannot express non-exportability for a file keychain.
    #[expect(deprecated)]
    let key = SecKey::generate(attributes.into_untyped())
        .map_err(|error| io::Error::other(error.to_string()))?;
    let properties = key.attributes();
    let nonexportable = properties
        .find(extractable.as_CFTypeRef().cast())
        .is_some_and(|value| *value == CFBoolean::false_value().as_CFTypeRef().cast());
    if !nonexportable {
        let _ = key.delete();
        return Err(io::Error::other(
            "Keychain did not create a non-exportable private key",
        ));
    }
    Ok(key)
}

fn machine_name() -> String {
    Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().chars().take(128).collect())
        .filter(|name: &String| !name.is_empty())
        .unwrap_or_else(|| "Mac".into())
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeSet,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
    };

    #[derive(Clone, Default)]
    struct MockTrust {
        trusted: Arc<Mutex<BTreeSet<String>>>,
        cancel: Arc<AtomicBool>,
    }

    impl Trust for MockTrust {
        fn available(&self) -> bool {
            true
        }

        fn contains(&self, root: &Root) -> io::Result<bool> {
            Ok(root
                .fingerprint()
                .is_some_and(|hash| self.trusted.lock().unwrap().contains(&hash)))
        }

        fn change(&self, root: &Root, _: &Path, add: bool) -> io::Result<()> {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(io::Error::other("Cancelled"));
            }
            let hash = root.fingerprint().unwrap();
            if add {
                self.trusted.lock().unwrap().insert(hash);
            } else {
                self.trusted.lock().unwrap().remove(&hash);
            }
            Ok(())
        }
    }

    struct TestKeychain(tempfile::TempDir, PathBuf);

    impl TestKeychain {
        fn new() -> Self {
            let folder = tempfile::tempdir().unwrap();
            let chain = folder.path().join("zentop-test.keychain");
            security(&["create-keychain", "-p", "isolated-test"], &chain);
            let chain = [chain.with_extension("keychain-db"), chain]
                .into_iter()
                .find(|path| path.exists())
                .unwrap();
            let test = Self(folder, chain);
            security(&["unlock-keychain", "-p", "isolated-test"], &test.1);
            test
        }
    }

    impl Drop for TestKeychain {
        fn drop(&mut self) {
            let _ = Command::new("/usr/bin/security")
                .arg("delete-keychain")
                .arg(&self.1)
                .output();
        }
    }

    fn security(args: &[&str], chain: &Path) {
        let result = Command::new("/usr/bin/security")
            .args(args)
            .arg(chain)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    async fn peer_public_key(
        config: Arc<tokio_rustls::rustls::ServerConfig>,
        client: Arc<tokio_rustls::rustls::ClientConfig>,
        host: &'static str,
    ) -> Vec<u8> {
        let (server_io, client_io) = tokio::io::duplex(16 * 1024);
        let (server, client) = tokio::join!(
            tokio_rustls::TlsAcceptor::from(config).accept(server_io),
            tokio_rustls::TlsConnector::from(client).connect(host.try_into().unwrap(), client_io),
        );
        assert!(server.is_ok());
        let client = client.unwrap();
        let chain = client.get_ref().1.peer_certificates().unwrap();
        let (_, leaf) = x509_parser::parse_x509_certificate(&chain[0]).unwrap();
        leaf.public_key().raw.to_vec()
    }

    #[tokio::test]
    async fn temporary_keychain_root_lifecycle() {
        let test = TestKeychain::new();
        let trust = MockTrust::default();
        let path = test.0.path().join("ca.json");
        let mut ca =
            Certificates::open_with(path.clone(), test.1.clone(), Box::new(trust.clone())).unwrap();
        assert_eq!(ca.inspect().0.state, CertificateState::None);
        ca.perform(CertificateAction::Create).unwrap();
        let (status, issuer) = ca.inspect();
        assert_eq!(
            status.state,
            CertificateState::Created,
            "{:?}",
            status.explanation
        );
        let root = ca.journal.active.as_ref().unwrap();
        let key = ca.find_key(&ca.keychain().unwrap(), root).unwrap().unwrap();
        assert!(
            key.external_representation().is_none(),
            "root key must not be exportable"
        );
        let root_public = KeychainKey::new(key).unwrap().subject_public_key_info();
        let tls = crate::Decryption::platform().unwrap();
        tls.set_hosts(["one.example".to_owned(), "two.example".to_owned()]);
        tls.set_authority(issuer);
        let leaf = |host| tls.leaf(host).unwrap().unwrap();
        let one = leaf("one.example");
        assert!(Arc::ptr_eq(&one, &leaf("one.example")));
        let two = leaf("two.example");
        let mut roots = tokio_rustls::rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(root.der.clone().unwrap()))
            .unwrap();
        let client = Arc::new(
            tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
                tokio_rustls::rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
        );
        let one_public = peer_public_key(one, client.clone(), "one.example").await;
        assert_ne!(one_public, root_public);
        assert_ne!(
            one_public,
            peer_public_key(two, client, "two.example").await
        );

        let old = status.fingerprint;
        ca.perform(CertificateAction::Trust).unwrap();
        assert_eq!(ca.inspect().0.state, CertificateState::Trusted);
        trust.cancel.store(true, Ordering::Relaxed);
        assert!(ca.perform(CertificateAction::Replace).is_err());
        assert!(ca.perform(CertificateAction::Remove).is_err());
        assert_eq!(ca.inspect().0.fingerprint, old);
        assert_eq!(ca.inspect().0.state, CertificateState::Trusted);
        trust.cancel.store(false, Ordering::Relaxed);
        ca.perform(CertificateAction::Replace).unwrap();
        assert_ne!(ca.inspect().0.fingerprint, old);
        assert_eq!(trust.trusted.lock().unwrap().len(), 1);
        let mut ca =
            Certificates::open_with(path, test.1.clone(), Box::new(trust.clone())).unwrap();
        assert_eq!(ca.inspect().0.state, CertificateState::Trusted);
        ca.journal.active.as_mut().unwrap().expires =
            (OffsetDateTime::now_utc() + Duration::days(10)).unix_timestamp();
        assert_eq!(ca.inspect().0.state, CertificateState::Expiring);
        security(&["lock-keychain"], &test.1);
        assert_eq!(ca.inspect().0.state, CertificateState::KeyUnavailable);
        security(&["unlock-keychain", "-p", "isolated-test"], &test.1);
        ca.journal.active.as_mut().unwrap().removing = true;
        assert_eq!(ca.inspect().0.state, CertificateState::PartlyRemoved);
        ca.perform(CertificateAction::Remove).unwrap();
        assert_eq!(ca.inspect().0.state, CertificateState::None);
        assert!(trust.trusted.lock().unwrap().is_empty());
    }
}
