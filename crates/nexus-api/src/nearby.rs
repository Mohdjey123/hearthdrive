//! Finding NEXUS devices on the same local network.
//!
//! Every few seconds each node with remote access on announces itself with a
//! small UDP broadcast (name, device id, public key). While someone has the
//! "Add a device" window open, the announcement also carries that pairing
//! link, so a nearby device can ask to pair with one click; the request
//! still has to be approved on the computer that made the link, as with a
//! link sent any other way.

use crate::state::SharedState;
use nexus_common::time::now_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The UDP port announcements are sent to and heard on.
pub const PORT: u16 = 47199;
const EVERY: Duration = Duration::from_secs(5);
/// A device not heard from for this long is no longer nearby.
const FORGET_AFTER: Duration = Duration::from_secs(20);
const MAX_BEACON: usize = 2048;
const MAX_SEEN: usize = 64;

/// What a node announces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Beacon {
    /// Always 1; anything else is not a NEXUS announcement.
    pub nexus: u8,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub device_type: String,
    pub key: String,
    #[serde(default)]
    pub version: String,
    /// A pairing link, while the device is waiting for one.
    #[serde(default)]
    pub invite: Option<String>,
}

struct Seen {
    beacon: Beacon,
    from: IpAddr,
    at: Instant,
}

/// A device heard on the local network.
#[derive(Debug, Clone, Serialize)]
pub struct NearbyDevice {
    pub id: String,
    pub name: String,
    pub device_type: String,
    pub address: String,
    pub fingerprint: Option<String>,
    pub version: String,
    /// Already paired with this computer.
    pub paired: bool,
    /// The pairing link it offers right now.
    pub invite: Option<String>,
}

/// Devices heard nearby, and the pairing link this node offers.
#[derive(Default)]
pub struct Nearby {
    seen: Mutex<HashMap<String, Seen>>,
    offer: Mutex<Option<(String, String, i64)>>,
}

impl Nearby {
    /// Offers a pairing link (by invite session) to nearby devices until it
    /// expires or is withdrawn.
    pub fn offer(&self, session_id: &str, link: &str, expires_at: i64) {
        *self.offer.lock().unwrap_or_else(|e| e.into_inner()) = Some((session_id.into(), link.into(), expires_at));
    }

    pub fn withdraw(&self, session_id: &str) {
        let mut offer = self.offer.lock().unwrap_or_else(|e| e.into_inner());
        if offer.as_ref().is_some_and(|(id, _, _)| id == session_id) {
            *offer = None;
        }
    }

    fn current_offer(&self) -> Option<String> {
        let mut offer = self.offer.lock().unwrap_or_else(|e| e.into_inner());
        match offer.as_ref() {
            Some((_, _, expires)) if *expires <= now_ms() => {
                *offer = None;
                None
            }
            Some((_, link, _)) => Some(link.clone()),
            None => None,
        }
    }

    fn heard(&self, beacon: Beacon, from: IpAddr) {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.retain(|_, s| s.at.elapsed() < FORGET_AFTER);
        if seen.len() >= MAX_SEEN && !seen.contains_key(&beacon.id) {
            return;
        }
        seen.insert(beacon.id.clone(), Seen { beacon, from, at: Instant::now() });
    }

    /// The devices heard recently, `paired` telling which are paired.
    pub fn list(&self, paired: impl Fn(&str, &str) -> bool) -> Vec<NearbyDevice> {
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<NearbyDevice> = seen
            .values()
            .filter(|s| s.at.elapsed() < FORGET_AFTER)
            .map(|s| {
                let b = &s.beacon;
                let is_paired = paired(&b.id, &b.key);
                NearbyDevice {
                    id: b.id.clone(),
                    name: b.name.clone(),
                    device_type: b.device_type.clone(),
                    address: s.from.to_string(),
                    fingerprint: nexus_devices::fingerprint(&b.key),
                    version: b.version.clone(),
                    paired: is_paired,
                    invite: if is_paired { None } else { b.invite.clone() },
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    }
}

/// Whether an announcement is well formed enough to show.
fn valid(b: &Beacon) -> bool {
    let id_ok = (1..=64).contains(&b.id.len()) && b.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let name = b.name.trim();
    b.nexus == 1
        && id_ok
        && !name.is_empty()
        && name.len() <= 64
        && !name.chars().any(char::is_control)
        && b.device_type.len() <= 16
        && b.version.len() <= 32
        && nexus_network::public_key(&b.key).is_some()
        && b.invite.as_ref().is_none_or(|l| l.len() <= 1500 && l.starts_with(nexus_devices::invite::SCHEME))
}

/// Where to send announcements: everyone on the local network, and the
/// broadcast address of each private network this node has an address on.
fn targets(hints: &[String]) -> Vec<SocketAddr> {
    let mut out = vec![SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, PORT))];
    for hint in hints {
        if let Ok(SocketAddr::V4(addr)) = hint.parse::<SocketAddr>()
            && addr.ip().is_private()
        {
            let [a, b, c, _] = addr.ip().octets();
            let target = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(a, b, c, 255), PORT));
            if !out.contains(&target) {
                out.push(target);
            }
        }
    }
    out
}

fn bind() -> std::io::Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_broadcast(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    tokio::net::UdpSocket::from_std(socket.into())
}

/// Announces this node and listens for others.
pub fn spawn(state: SharedState) {
    if std::env::var_os("NEXUS_NO_NEARBY").is_some() {
        return;
    }
    tokio::spawn(async move {
        let socket = match bind() {
            Ok(s) => s,
            Err(e) => {
                tracing::info!(error = %e, port = PORT, "cannot look for devices on the local network");
                return;
            }
        };
        let mut tick = tokio::time::interval(EVERY);
        let mut buf = vec![0u8; MAX_BEACON + 1];
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if !state.net.remote_enabled() {
                        continue;
                    }
                    let Ok(me) = state.db.get_device(&state.node.device_id).await else { continue };
                    let beacon = Beacon {
                        nexus: 1,
                        id: me.id,
                        name: me.name,
                        device_type: me.device_type,
                        key: state.identity.public_key().to_string(),
                        version: nexus_common::VERSION.into(),
                        invite: state.nearby.current_offer(),
                    };
                    let Ok(bytes) = serde_json::to_vec(&beacon) else { continue };
                    for target in targets(&state.net.advertised()) {
                        let _ = socket.send_to(&bytes, target).await;
                    }
                }
                got = socket.recv_from(&mut buf) => {
                    let Ok((n, from)) = got else { continue };
                    if n > MAX_BEACON {
                        continue;
                    }
                    let Ok(beacon) = serde_json::from_slice::<Beacon>(&buf[..n]) else { continue };
                    if beacon.id == state.node.device_id || !valid(&beacon) {
                        continue;
                    }
                    state.nearby.heard(beacon, from.ip());
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beacon() -> Beacon {
        Beacon {
            nexus: 1,
            id: "dev-1".into(),
            name: "Laptop".into(),
            device_type: "windows".into(),
            key: nexus_devices::NodeIdentity::generate().public_key().to_string(),
            version: "0.1.4".into(),
            invite: None,
        }
    }

    #[test]
    fn checks_announcements() {
        assert!(valid(&beacon()));
        assert!(!valid(&Beacon { nexus: 2, ..beacon() }));
        assert!(!valid(&Beacon { name: "\u{7}".into(), ..beacon() }));
        assert!(!valid(&Beacon { key: "nope".into(), ..beacon() }));
        assert!(!valid(&Beacon { invite: Some("https://evil".into()), ..beacon() }));
    }

    #[test]
    fn broadcasts_on_private_networks() {
        let t = targets(&["192.168.1.20:4000".into(), "8.8.8.8:1".into(), "relay:https://r".into()]);
        assert_eq!(t.len(), 2);
        assert_eq!(t[1], "192.168.1.255:47199".parse().unwrap());
    }

    #[test]
    fn lists_and_forgets_offers() {
        let nearby = Nearby::default();
        nearby.heard(Beacon { invite: Some("nexus-pair:x".into()), ..beacon() }, IpAddr::V4(Ipv4Addr::LOCALHOST));
        let list = nearby.list(|_, _| false);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].invite.as_deref(), Some("nexus-pair:x"));
        assert!(nearby.list(|_, _| true)[0].invite.is_none(), "paired devices offer nothing");
        nearby.offer("s", "nexus-pair:y", now_ms() - 1);
        assert!(nearby.current_offer().is_none(), "an expired link is not offered");
    }
}
