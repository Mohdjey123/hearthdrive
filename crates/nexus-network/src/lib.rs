//! Built-in peer-to-peer networking.
//!
//! Each node runs an [iroh](https://iroh.computer) endpoint whose key is the
//! node's own device key, so a paired device's public key is also its network
//! address. Connections are QUIC with TLS bound to those keys: a node always
//! knows which device it is talking to. Endpoints connect directly when they
//! can (same network, or through NAT with hole punching) and otherwise through
//! a relay server, which only forwards encrypted packets. No separate VPN
//! client, account or port forwarding is needed.
//!
//! The HTTP API itself still listens on loopback only. Other nodes reach it
//! through this endpoint, and `nexus-api` decides what they may call.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::{Endpoint, EndpointAddr, PublicKey, RelayMode, RelayUrl, SecretKey, TransportAddr, Watcher};
use serde::Serialize;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use iroh::PublicKey as PeerKey;
pub use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream};

/// Application protocol spoken on HearthDrive connections: HTTP/1.1, one request
/// per bidirectional stream.
pub const ALPN: &[u8] = b"nexus/http/1";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
/// How long the endpoint may look for a relay before reporting "local only".
const STARTUP_GRACE: Duration = Duration::from_secs(20);
const MAX_HINTS: usize = 8;

/// Which relay servers to use when a direct connection is not possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayChoice {
    /// The public relays run by the iroh project (number 0).
    Default,
    /// Your own `iroh-relay` servers.
    Custom(Vec<String>),
    /// Direct connections only (same network or reachable addresses).
    Disabled,
}

#[derive(Debug, Clone)]
pub struct P2pConfig {
    pub relay: RelayChoice,
    /// Publish this node's relay address under its key through the iroh DNS
    /// service, so peers can find it by key alone after an address change.
    pub address_lookup: bool,
    /// Preferred UDP port. A random port is used if it is taken.
    pub port: Option<u16>,
    /// Bind 127.0.0.1 only (tests and two nodes on one computer).
    pub loopback_only: bool,
    /// Ask the router for a port mapping (UPnP / NAT-PMP / PCP).
    pub portmapper: bool,
    /// Diagnostics: never use direct paths, only the relay
    /// (`NEXUS_RELAY_ONLY=1`), to check what a far-away device experiences.
    pub relay_only: bool,
}

impl P2pConfig {
    /// Defaults, overridden by `NEXUS_RELAY` (`default`, `off`, or a
    /// comma-separated list of relay URLs) and `NEXUS_P2P_PORT`.
    pub fn from_env(default_port: u16) -> Self {
        let relay = match std::env::var("NEXUS_RELAY").ok().as_deref().map(str::trim) {
            None | Some("") | Some("default") => RelayChoice::Default,
            Some("off") | Some("none") | Some("disabled") => RelayChoice::Disabled,
            Some(list) => RelayChoice::Custom(
                list.split(',').map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).collect(),
            ),
        };
        let port = std::env::var("NEXUS_P2P_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(default_port);
        Self {
            address_lookup: relay == RelayChoice::Default,
            relay,
            port: Some(port),
            loopback_only: false,
            portmapper: true,
            relay_only: matches!(std::env::var("NEXUS_RELAY_ONLY").as_deref(), Ok("1") | Ok("true")),
        }
    }

    /// Loopback, no relay, no lookup: for tests.
    pub fn loopback() -> Self {
        Self {
            relay: RelayChoice::Disabled,
            address_lookup: false,
            port: None,
            loopback_only: true,
            portmapper: false,
            relay_only: false,
        }
    }

    fn relay_mode(&self) -> Result<RelayMode, String> {
        Ok(match &self.relay {
            RelayChoice::Default => RelayMode::Default,
            RelayChoice::Disabled => RelayMode::Disabled,
            RelayChoice::Custom(urls) => {
                let parsed = urls
                    .iter()
                    .map(|u| u.parse::<RelayUrl>().map_err(|e| format!("invalid relay URL {u}: {e}")))
                    .collect::<Result<Vec<_>, _>>()?;
                RelayMode::custom(parsed)
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkState {
    /// The owner turned remote access off.
    Off,
    /// Looking for a relay server.
    Starting,
    /// Reachable from anywhere: connected to a relay, direct paths as available.
    Online,
    /// No relay: only devices that can reach this computer's addresses
    /// directly (usually the same local network) can connect.
    LocalOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkStatus {
    /// Always `p2p`.
    pub provider: &'static str,
    pub display_name: String,
    pub state: NetworkState,
    /// Other devices can currently connect to this node.
    pub connected: bool,
    pub detail: String,
    /// `default`, `custom` or `disabled`.
    pub relay_mode: &'static str,
    /// The home relay this node is reachable through.
    pub relay_url: Option<String>,
    pub relay_error: Option<String>,
    /// Local and public `ip:port` addresses peers may connect to directly.
    pub direct_addresses: Vec<String>,
    pub address_lookup: bool,
}

/// How a connection to a peer currently flows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Route {
    /// `direct` or `relay`.
    pub kind: &'static str,
    /// The peer's address on the selected path (`ip:port` or the relay URL).
    pub remote: String,
    pub rtt_ms: u64,
}

/// This node's endpoint, plus a cache of outgoing connections.
pub struct P2pNode {
    endpoint: Endpoint,
    config: P2pConfig,
    started: Instant,
    conns: Mutex<HashMap<PublicKey, Connection>>,
    /// Connections peers opened to this node, while they are served.
    incoming: Mutex<HashMap<usize, Connection>>,
}

/// Decodes a HearthDrive public key (base64url of 32 bytes).
pub fn public_key(b64: &str) -> Option<PublicKey> {
    let bytes = URL_SAFE_NO_PAD.decode(b64.trim()).ok()?;
    let bytes: [u8; 32] = bytes.as_slice().try_into().ok()?;
    PublicKey::from_bytes(&bytes).ok()
}

/// Encodes a key the way HearthDrive stores device keys.
pub fn key_string(key: &PublicKey) -> String {
    URL_SAFE_NO_PAD.encode(key.as_bytes())
}

/// A short label for a key in logs and records (`p2p:` + 12 characters).
pub fn key_short(b64: &str) -> String {
    format!("p2p:{}", b64.chars().take(12).collect::<String>())
}

/// Turns address hints (`relay:<url>` or `ip:port`) into a dialable address.
pub fn endpoint_addr(key: PublicKey, hints: &[String]) -> EndpointAddr {
    let addrs = hints.iter().filter_map(|h| match h.strip_prefix("relay:") {
        Some(url) => url.parse::<RelayUrl>().ok().map(TransportAddr::Relay),
        None => h.parse::<SocketAddr>().ok().map(TransportAddr::Ip),
    });
    EndpointAddr::from_parts(key, addrs)
}

/// Whether an address hint is well formed (used to filter untrusted input).
pub fn valid_hint(hint: &str) -> bool {
    if hint.len() > 255 {
        return false;
    }
    match hint.strip_prefix("relay:") {
        Some(url) => url.starts_with("https://") && url.parse::<RelayUrl>().is_ok(),
        None => hint.parse::<SocketAddr>().is_ok(),
    }
}

fn useful_direct(addr: &SocketAddr, loopback_only: bool) -> bool {
    match addr.ip() {
        _ if loopback_only => addr.ip().is_loopback(),
        IpAddr::V4(ip) => !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified(),
        IpAddr::V6(ip) => !ip.is_loopback() && !ip.is_unspecified() && !ip.is_unicast_link_local(),
    }
}

impl P2pNode {
    /// Binds the endpoint with this node's device key.
    pub async fn start(secret: [u8; 32], config: P2pConfig) -> Result<Self, String> {
        // HTTPS clients inside iroh (address lookup) and elsewhere in the
        // process use the process-wide TLS provider; make sure there is one.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let endpoint = match bind(secret, &config, config.port).await {
            Ok(ep) => ep,
            Err(e) if config.port.is_some() => {
                tracing::warn!(port = ?config.port, error = %e, "preferred UDP port unavailable; using a random one");
                bind(secret, &config, None).await?
            }
            Err(e) => return Err(e),
        };
        tracing::info!(sockets = ?endpoint.bound_sockets(), relay = ?config.relay, "peer-to-peer endpoint ready");
        Ok(Self { endpoint, config, started: Instant::now(), conns: Mutex::new(HashMap::new()), incoming: Mutex::new(HashMap::new()) })
    }

    pub fn id(&self) -> PublicKey {
        self.endpoint.id()
    }

    /// Ways to reach this node: its home relay first, then direct addresses.
    pub fn hints(&self) -> Vec<String> {
        let addr = self.endpoint.addr();
        let mut out: Vec<String> = addr.relay_urls().map(|u| format!("relay:{u}")).collect();
        out.extend(
            addr.ip_addrs()
                .filter(|a| useful_direct(a, self.config.loopback_only))
                .map(|a| a.to_string()),
        );
        out.truncate(MAX_HINTS);
        out
    }

    /// Current reachability, for the UI. `enabled` is the owner's switch.
    pub fn status(&self, enabled: bool) -> NetworkStatus {
        let relays = self.endpoint.home_relay_status().get();
        let home = relays.iter().find(|r| r.is_connected()).or(relays.first());
        let relay_url = home.map(|r| r.url().to_string());
        let relay_connected = relays.iter().any(|r| r.is_connected());
        let relay_error = if relay_connected { None } else { relays.iter().find_map(|r| r.last_error()).map(|e| e.to_string()) };
        let direct_addresses: Vec<String> = self
            .endpoint
            .addr()
            .ip_addrs()
            .filter(|a| useful_direct(a, self.config.loopback_only))
            .map(|a| a.to_string())
            .collect();
        let relay_mode = match self.config.relay {
            RelayChoice::Default => "default",
            RelayChoice::Custom(_) => "custom",
            RelayChoice::Disabled => "disabled",
        };

        let (state, detail) = if !enabled {
            (NetworkState::Off, "Remote access is off. Paired devices cannot connect to this device.".to_string())
        } else if relay_connected {
            (
                NetworkState::Online,
                "Paired devices can reach this device from any network. Connections go direct when possible, otherwise through an encrypted relay.".into(),
            )
        } else if self.config.relay != RelayChoice::Disabled && self.started.elapsed() < STARTUP_GRACE {
            (NetworkState::Starting, "Connecting to the relay network…".into())
        } else if self.config.relay == RelayChoice::Disabled {
            (
                NetworkState::LocalOnly,
                "Relays are turned off. Only devices that can reach this device directly (usually on the same network) can connect.".into(),
            )
        } else {
            (
                NetworkState::LocalOnly,
                "No relay server is reachable, so only devices on the same network can connect right now. Check the internet connection or firewall.".into(),
            )
        };
        NetworkStatus {
            provider: "p2p",
            display_name: match self.config.relay {
                RelayChoice::Custom(_) => "Built-in (your relay)".into(),
                _ => "Built-in peer-to-peer".into(),
            },
            connected: enabled && (relay_connected || !direct_addresses.is_empty()),
            state,
            detail,
            relay_mode,
            relay_url,
            relay_error,
            direct_addresses,
            address_lookup: self.config.address_lookup,
        }
    }

    /// Resolves when this node's addresses or relay connection change.
    pub async fn changed(&self) {
        let mut addr = self.endpoint.watch_addr();
        let mut relay = self.endpoint.home_relay_status();
        tokio::select! {
            r = addr.updated() => { if r.is_err() { std::future::pending::<()>().await } }
            r = relay.updated() => { if r.is_err() { std::future::pending::<()>().await } }
        }
    }

    /// An open connection to `key`, reusing a cached one.
    pub async fn connect(&self, key: PublicKey, hints: &[String]) -> Result<Connection, String> {
        if key == self.endpoint.id() {
            return Err("cannot connect to this node itself".into());
        }
        if let Some(conn) = self.cached(&key) {
            return Ok(conn);
        }
        let addr = endpoint_addr(key, hints);
        if addr.is_empty() && !self.config.address_lookup {
            return Err("no known address".into());
        }
        let conn = match tokio::time::timeout(CONNECT_TIMEOUT, self.endpoint.connect(addr, ALPN)).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(e)) => return Err(e.to_string()),
            Err(_) => return Err("timed out".into()),
        };
        self.conns.lock().unwrap_or_else(|e| e.into_inner()).insert(key, conn.clone());
        Ok(conn)
    }

    fn cached(&self, key: &PublicKey) -> Option<Connection> {
        let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        match conns.get(key) {
            Some(c) if c.close_reason().is_none() => Some(c.clone()),
            Some(_) => {
                conns.remove(key);
                None
            }
            None => None,
        }
    }

    /// Drops the cached connection to `key` (after a failure, or on removal).
    pub fn forget(&self, key: &PublicKey) {
        if let Some(conn) = self.conns.lock().unwrap_or_else(|e| e.into_inner()).remove(key) {
            conn.close(0u32.into(), b"forgotten");
        }
    }

    /// Remembers a connection a peer opened to this node, so its traffic
    /// counts; [`P2pNode::untrack`] once it is no longer served.
    pub fn track(&self, conn: &Connection) {
        self.incoming.lock().unwrap_or_else(|e| e.into_inner()).insert(conn.stable_id(), conn.clone());
    }

    pub fn untrack(&self, conn: &Connection) {
        self.incoming.lock().unwrap_or_else(|e| e.into_inner()).remove(&conn.stable_id());
    }

    /// Bytes received from and sent to `key` over the connections open now
    /// (both directions), as `(received, sent)`. `None` when not connected.
    pub fn traffic(&self, key: &PublicKey) -> Option<(u64, u64)> {
        let mut conns: Vec<Connection> = self.cached(key).into_iter().collect();
        conns.extend(
            self.incoming
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|c| c.remote_id() == *key && c.close_reason().is_none())
                .cloned(),
        );
        if conns.is_empty() {
            return None;
        }
        Some(conns.iter().fold((0, 0), |(rx, tx), c| {
            let stats = c.stats();
            (rx + stats.udp_rx.bytes, tx + stats.udp_tx.bytes)
        }))
    }

    /// How traffic to `key` currently flows, if connected.
    pub fn route(&self, key: &PublicKey) -> Option<Route> {
        let conn = self.cached(key)?;
        route_of(&conn)
    }

    /// The next incoming connection attempt. `None` once the endpoint closed.
    pub async fn accept(&self) -> Option<Incoming> {
        self.endpoint.accept().await
    }

    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}

/// Describes the selected path of a connection.
pub fn route_of(conn: &Connection) -> Option<Route> {
    let paths = conn.paths();
    let path = paths.iter().find(|p| p.is_selected())?;
    let remote = match path.remote_addr() {
        TransportAddr::Relay(url) => url.to_string(),
        TransportAddr::Ip(addr) => addr.to_string(),
        other => format!("{other:?}"),
    };
    Some(Route {
        kind: if path.is_relay() { "relay" } else { "direct" },
        remote,
        rtt_ms: path.rtt().as_millis() as u64,
    })
}

async fn bind(secret: [u8; 32], config: &P2pConfig, port: Option<u16>) -> Result<Endpoint, String> {
    let mut builder = if config.address_lookup {
        Endpoint::builder(presets::N0)
    } else {
        Endpoint::builder(presets::Minimal)
    };
    builder = builder
        .secret_key(SecretKey::from_bytes(&secret))
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(config.relay_mode()?);
    if !config.portmapper {
        builder = builder.portmapper_config(PortmapperConfig::Disabled);
    }
    if config.relay_only {
        builder = builder.clear_ip_transports();
    } else if config.loopback_only {
        builder = builder
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .map_err(|e| e.to_string())?
            .bind_addr("[::1]:0")
            .map_err(|e| e.to_string())?;
    } else if let Some(port) = port {
        builder = builder
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port))
            .map_err(|e| e.to_string())?;
    }
    builder.bind().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_round_trip_and_validate() {
        let key = SecretKey::from_bytes(&[7u8; 32]).public();
        let encoded = key_string(&key);
        assert_eq!(public_key(&encoded), Some(key));
        assert!(public_key("not-a-key").is_none());

        let hints = vec![
            "relay:https://relay.example.com/".to_string(),
            "192.168.1.20:47100".to_string(),
            "garbage".to_string(),
        ];
        assert!(valid_hint(&hints[0]));
        assert!(valid_hint(&hints[1]));
        assert!(!valid_hint(&hints[2]));
        assert!(!valid_hint("relay:http://plain.example.com/"));
        let addr = endpoint_addr(key, &hints);
        assert_eq!(addr.relay_urls().count(), 1);
        assert_eq!(addr.ip_addrs().count(), 1);
    }

    #[tokio::test]
    async fn two_endpoints_connect_by_key_over_loopback() {
        let a = P2pNode::start([1u8; 32], P2pConfig::loopback()).await.unwrap();
        let b = P2pNode::start([2u8; 32], P2pConfig::loopback()).await.unwrap();
        let hints = a.hints();
        assert!(!hints.is_empty());

        let server = tokio::spawn(async move {
            let conn = a.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();
            let msg = recv.read_to_end(64).await.unwrap();
            send.write_all(&msg).await.unwrap();
            send.finish().unwrap();
            let _ = conn.closed().await;
            conn.remote_id()
        });

        let a_key = SecretKey::from_bytes(&[1u8; 32]).public();
        let conn = b.connect(a_key, &hints).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        send.write_all(b"hello").await.unwrap();
        send.finish().unwrap();
        assert_eq!(recv.read_to_end(64).await.unwrap(), b"hello");
        assert_eq!(route_of(&conn).unwrap().kind, "direct");
        b.forget(&a_key);
        assert_eq!(server.await.unwrap(), b.id());
    }
}
