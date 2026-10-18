//! Native-only consumer credential recovery and hosted-link address selection.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

use super::ApiError;

const SERVICE: &str = "com.snenk.snenkbot.api.admin";

/// Stores recoverable administrative tokens separately from consumer-accessible API secrets.
pub trait ConsumerCredentialStore: Send + Sync {
    /// Reads one private credential slot without creating or rotating a token.
    fn read(&self, slot: &str) -> Result<Option<String>, ApiError>;
    /// Stores a fresh token before its digest is committed to the consumer registry.
    fn write(&self, slot: &str, token: &str) -> Result<(), ApiError>;
    /// Removes an obsolete slot after the registry no longer authenticates its token.
    fn remove(&self, slot: &str) -> Result<(), ApiError>;
}

/// Uses a distinct native keyring service that the consumer secrets API never opens.
pub struct SystemConsumerCredentialStore;

impl ConsumerCredentialStore for SystemConsumerCredentialStore {
    /// Reads a previously retained token from its private administrative account.
    fn read(&self, slot: &str) -> Result<Option<String>, ApiError> {
        match entry(slot)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(ApiError::Credential),
        }
    }

    /// Writes only the new slot, leaving the previous authenticated token untouched.
    fn write(&self, slot: &str, token: &str) -> Result<(), ApiError> {
        entry(slot)?
            .set_password(token)
            .map_err(|_| ApiError::Credential)
    }

    /// Deletes an obsolete account and tolerates a credential already removed elsewhere.
    fn remove(&self, slot: &str) -> Result<(), ApiError> {
        match entry(slot)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(ApiError::Credential),
        }
    }
}

/// Opens an administrative credential account without performing a credential operation.
fn entry(slot: &str) -> Result<keyring::Entry, ApiError> {
    keyring::Entry::new(SERVICE, slot).map_err(|_| ApiError::Credential)
}

/// Formats the actual listener address, resolving wildcard binds through the local route table.
pub(super) fn hosting_url(address: SocketAddr, token: &str) -> Result<String, ApiError> {
    hosting_url_with_route(address, token, routed_address)
}

/// Applies injectable route discovery so URL formatting tests never open a network socket.
fn hosting_url_with_route(
    address: SocketAddr,
    token: &str,
    route: impl FnOnce(IpAddr) -> Result<IpAddr, ApiError>,
) -> Result<String, ApiError> {
    let ip = if address.ip().is_unspecified() {
        route(address.ip())?
    } else {
        address.ip()
    };
    if ip.is_unspecified() {
        return Err(ApiError::Invalid(
            "Could not determine a reachable API address.".into(),
        ));
    }
    Ok(format!(
        "http://{}/files/{token}/",
        SocketAddr::new(ip, address.port())
    ))
}

/// Reads the operating system's outbound interface choice without sending a UDP packet.
fn routed_address(bind: IpAddr) -> Result<IpAddr, ApiError> {
    let destination = match bind {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888)),
    };
    let socket = UdpSocket::bind(SocketAddr::new(bind, 0)).map_err(|_| route_error())?;
    socket
        .connect(SocketAddr::new(destination, 80))
        .map_err(|_| route_error())?;
    let ip = socket.local_addr().map_err(|_| route_error())?.ip();
    if ip.is_unspecified() || ip.is_loopback() {
        return Err(route_error());
    }
    Ok(ip)
}

/// Gives an actionable error without exposing credentials or local socket internals.
fn route_error() -> ApiError {
    ApiError::Invalid(
        "Could not determine a LAN address. Configure a specific API host address and try again."
            .into(),
    )
}

#[cfg(test)]
/// An injected in-memory credential backend for prompt-free persistence and failure tests.
#[derive(Default)]
pub(super) struct MemoryConsumerCredentialStore {
    pub tokens: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    pub fail_writes: std::sync::atomic::AtomicBool,
    pub fail_removals: std::sync::atomic::AtomicBool,
    pub reads: std::sync::atomic::AtomicUsize,
    pub after_write: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

#[cfg(test)]
impl ConsumerCredentialStore for MemoryConsumerCredentialStore {
    /// Returns an in-memory token while counting explicit recovery reads.
    fn read(&self, slot: &str) -> Result<Option<String>, ApiError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.tokens.lock().unwrap().get(slot).cloned())
    }

    /// Inserts an isolated token or simulates a credential-storage failure.
    fn write(&self, slot: &str, token: &str) -> Result<(), ApiError> {
        if self.fail_writes.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ApiError::Credential);
        }
        self.tokens
            .lock()
            .unwrap()
            .insert(slot.into(), token.into());
        if let Some(hook) = self.after_write.lock().unwrap().take() {
            hook();
        }
        Ok(())
    }

    /// Deletes an isolated slot or simulates an obsolete-account cleanup failure.
    fn remove(&self, slot: &str) -> Result<(), ApiError> {
        if self.fail_removals.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ApiError::Credential);
        }
        self.tokens.lock().unwrap().remove(slot);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Covers explicit addresses and wildcard routing without accessing the network or keyring.
    #[test]
    fn hosting_urls_preserve_ports_and_bracket_ipv6() {
        assert_eq!(
            hosting_url_with_route("127.0.0.1:38764".parse().unwrap(), "sb_token", |_| panic!(
                "unexpected route lookup"
            ))
            .unwrap(),
            "http://127.0.0.1:38764/files/sb_token/"
        );
        assert_eq!(
            hosting_url_with_route("[::1]:4444".parse().unwrap(), "sb_token", |_| panic!(
                "unexpected route lookup"
            ))
            .unwrap(),
            "http://[::1]:4444/files/sb_token/"
        );
        assert_eq!(
            hosting_url_with_route("0.0.0.0:1234".parse().unwrap(), "sb_token", |_| Ok(
                "192.168.1.20".parse().unwrap()
            ))
            .unwrap(),
            "http://192.168.1.20:1234/files/sb_token/"
        );
        assert!(
            hosting_url_with_route("[::]:1234".parse().unwrap(), "sb_token", |_| Err(
                route_error()
            ))
            .is_err()
        );
    }
}
