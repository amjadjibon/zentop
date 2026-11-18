//! Explicit OpenSSH destinations saved by the desktop dashboard.
use crate::monitoring_preferences::{config_directory, lock_settings, write_settings};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::File,
    io::{self, Read},
    net::Ipv6Addr,
    path::{Path, PathBuf},
};

pub const MAX_HOSTS: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub name: String,
    pub destination: String,
}

impl Host {
    pub fn new(name: &str, destination: &str) -> Result<Self, String> {
        let host = Self {
            name: name.trim().into(),
            destination: destination.trim().into(),
        };
        host.validate()?;
        Ok(host)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty()
            || crate::ssh_stream::safe_display(&self.name, 64) != self.name
        {
            return Err("Enter a name with 1–64 visible characters.".into());
        }
        let (user, host) = self
            .destination
            .split_once('@')
            .map_or((None, self.destination.as_str()), |(user, host)| {
                (Some(user), host)
            });
        if self.destination.len() > 320
            || user.is_some_and(|user| !valid_token(user))
            || !(valid_token(host) || host.parse::<Ipv6Addr>().is_ok())
        {
            return Err("Use an SSH alias, hostname, IP address, or user@host. Set custom ports in your SSH config.".into());
        }
        Ok(())
    }
}

// In particular, reject option-like names, whitespace, percent expansions,
// and shell metacharacters before handing a destination to OpenSSH.
fn valid_token(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}

const MAX_SETTINGS_BYTES: u64 = 32 * 1024;
const MAX_LEGACY_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Default)]
pub struct Store {
    path: Option<PathBuf>,
    hosts: Vec<Host>,
}

impl Store {
    pub fn open() -> (Self, Option<String>) {
        match config_directory() {
            Some(dir) => Self::open_at(dir.join("ssh-hosts.json")),
            None => (
                Self::default(),
                Some("SSH host settings location unavailable.".into()),
            ),
        }
    }

    fn open_at(path: PathBuf) -> (Self, Option<String>) {
        let (hosts, error) = match Self::load_or_import(&path) {
            Ok(hosts) => (hosts, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        (
            Self {
                path: Some(path),
                hosts,
            },
            error,
        )
    }

    pub fn hosts(&self) -> &[Host] {
        &self.hosts
    }

    pub fn add(&mut self, host: Host) -> Result<(), String> {
        self.change(|hosts| hosts.push(host))
    }

    pub fn remove(&mut self, destination: &str) -> Result<(), String> {
        self.change(|hosts| hosts.retain(|host| host.destination != destination))
    }

    fn load_or_import(path: &Path) -> Result<Vec<Host>, String> {
        if let Some(hosts) = read_hosts(path)? {
            return Ok(hosts);
        }
        let legacy_path = path.with_file_name("monitoring.json");
        // Avoid creating a settings directory for a new profile until it saves hosts.
        if !legacy_path.try_exists().map_err(|e| e.to_string())? {
            return Ok(Vec::new());
        }
        let _lock = lock_settings(path)?;
        if let Some(hosts) = read_hosts(path)? {
            return Ok(hosts);
        }
        let Some(hosts) = read_legacy_hosts(&legacy_path)? else {
            return Ok(Vec::new());
        };
        save_hosts(path, &hosts)?;
        Ok(hosts)
    }

    fn change(&mut self, edit: impl FnOnce(&mut Vec<Host>)) -> Result<(), String> {
        let path = self
            .path
            .as_deref()
            .ok_or("SSH host settings location unavailable.")?;
        let _lock = lock_settings(path)?;
        let mut hosts = match read_hosts(path)? {
            Some(hosts) => hosts,
            None => read_legacy_hosts(&path.with_file_name("monitoring.json"))?.unwrap_or_default(),
        };
        edit(&mut hosts);
        save_hosts(path, &hosts)?;
        self.hosts = hosts;
        Ok(())
    }
}

fn read_settings<T: serde::de::DeserializeOwned>(
    path: &Path,
    limit: u64,
) -> Result<Option<T>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not read SSH host settings: {error}")),
    };
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("SSH host settings exceed the size limit.".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("Invalid SSH host settings: {error}"))
}

fn read_hosts(path: &Path) -> Result<Option<Vec<Host>>, String> {
    let hosts: Option<Vec<Host>> = read_settings(path, MAX_SETTINGS_BYTES)?;
    if let Some(hosts) = &hosts {
        validate_hosts(hosts)?;
    }
    Ok(hosts)
}

fn read_legacy_hosts(path: &Path) -> Result<Option<Vec<Host>>, String> {
    #[derive(Deserialize)]
    struct Legacy {
        ssh_hosts: Option<Vec<Host>>,
    }
    Ok(read_settings::<Legacy>(path, MAX_LEGACY_BYTES)?.and_then(|settings| settings.ssh_hosts))
}

fn validate_hosts(hosts: &[Host]) -> Result<(), String> {
    if hosts.len() > MAX_HOSTS {
        return Err(format!("At most {MAX_HOSTS} SSH hosts can be saved."));
    }
    let mut destinations = HashSet::new();
    for host in hosts {
        host.validate()?;
        if !destinations.insert(&host.destination) {
            return Err("That SSH destination is already saved.".into());
        }
    }
    Ok(())
}

fn save_hosts(path: &Path, hosts: &[Host]) -> Result<(), String> {
    validate_hosts(hosts)?;
    let bytes = serde_json::to_vec_pretty(hosts).map_err(|error| error.to_string())?;
    write_settings(path, &bytes)
        .map_err(|error| format!("Could not save SSH host settings: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "zentop-test-hosts-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn open(&self) -> Store {
            let (store, error) = Store::open_at(self.0.join("ssh-hosts.json"));
            assert_eq!(error, None);
            store
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn old_preferences_writer_cannot_erase_imported_hosts() {
        let fixture = Fixture::new();
        let monitoring = fixture.0.join("monitoring.json");
        // An older binary reads its known fields and discards new keys on save.
        #[derive(Serialize, Deserialize)]
        struct OldSettings {
            version: u32,
            views: Vec<String>,
        }
        let host = Host::new("Lab", "lab").unwrap();
        let legacy = serde_json::json!({"version":1, "views": [], "ssh_hosts": [&host]});
        std::fs::write(&monitoring, serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(fixture.open().hosts(), std::slice::from_ref(&host));
        let mut old: OldSettings =
            serde_json::from_slice(&std::fs::read(&monitoring).unwrap()).unwrap();
        old.views.push("Existing view".into());
        std::fs::write(&monitoring, serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(fixture.open().hosts(), &[host]);
    }

    #[test]
    fn removing_last_host_does_not_reimport_legacy_hosts() {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.0.join("monitoring.json"),
            r#"{"ssh_hosts":[{"name":"Lab","destination":"lab"}]}"#,
        )
        .unwrap();
        let mut store = fixture.open();
        assert!(store.add(Host::new("Duplicate", "lab").unwrap()).is_err());
        store.remove("lab").unwrap();
        assert!(fixture.open().hosts().is_empty());
    }

    #[test]
    fn parses_explicit_destinations_without_ssh_options_or_expansions() {
        for good in [
            "home-server",
            "alex@192.0.2.1",
            "alex@host.example",
            "alex@2001:db8::1",
        ] {
            assert!(Host::new("Lab", good).is_ok(), "{good}");
        }
        for bad in [
            "-oProxyCommand=x",
            "user@-host",
            "host:22",
            "user@@host",
            "user@host;id",
            "$(id)",
            "%n",
            "host\nother",
            "ssh://user@host",
            "user'@host",
        ] {
            assert!(Host::new("Lab", bad).is_err(), "{bad}");
        }
    }
}
