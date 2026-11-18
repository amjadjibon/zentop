//! Explicit OpenSSH destinations saved by the desktop dashboard.
use serde::{Deserialize, Serialize};
use std::net::Ipv6Addr;

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

#[cfg(test)]
mod tests {
    use super::*;

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
