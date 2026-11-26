//! Power: waking sleeping computers over the network (Wake-on-LAN), keeping
//! this computer awake while it transfers files, and noticing when it runs
//! on battery.

use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

/// The Wake-on-LAN port (the "discard" port most network cards listen on).
const WOL_PORT: u16 = 9;
const MAX_MACS: usize = 8;

/// What another computer needs to wake this one: the network cards' MAC
/// addresses and the broadcast address of each local network.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WakeInfo {
    pub macs: Vec<String>,
    pub broadcasts: Vec<String>,
}

impl WakeInfo {
    pub fn is_empty(&self) -> bool {
        self.macs.is_empty()
    }

    /// Keeps only well formed addresses, at most a few of each.
    pub fn cleaned(self) -> Self {
        let macs = self.macs.into_iter().filter_map(|m| parse_mac(&m).map(format_mac)).take(MAX_MACS).collect();
        let broadcasts = self
            .broadcasts
            .into_iter()
            .filter(|b| b.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_private() || ip.is_broadcast()))
            .take(MAX_MACS)
            .collect();
        Self { macs, broadcasts }
    }
}

/// This computer's wired and wireless network cards that are up.
pub fn wake_info() -> WakeInfo {
    let mut info = WakeInfo::default();
    for iface in netdev::get_interfaces() {
        if iface.is_loopback() || !iface.is_up() {
            continue;
        }
        let Some(mac) = iface.mac_addr.map(|m| m.to_string()).and_then(|m| parse_mac(&m)) else { continue };
        if mac == [0; 6] {
            continue;
        }
        let nets: Vec<_> = iface.ipv4.iter().filter(|n| n.addr().is_private()).collect();
        if nets.is_empty() {
            continue;
        }
        let mac = format_mac(mac);
        if !info.macs.contains(&mac) {
            info.macs.push(mac);
        }
        for net in nets {
            let b = net.broadcast().to_string();
            if !info.broadcasts.contains(&b) {
                info.broadcasts.push(b);
            }
        }
    }
    info.macs.truncate(MAX_MACS);
    info.broadcasts.truncate(MAX_MACS);
    info
}

pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        if p.len() != 2 {
            return None;
        }
        out[i] = u8::from_str_radix(p, 16).ok()?;
    }
    Some(out)
}

fn format_mac(m: [u8; 6]) -> String {
    m.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// The "magic packet": six 0xFF bytes, then the MAC address 16 times.
pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut packet = [0xFFu8; 102];
    for i in 0..16 {
        packet[6 + i * 6..12 + i * 6].copy_from_slice(&mac);
    }
    packet
}

/// Sends magic packets for `info` on the local networks. Returns how many
/// packets went out.
pub async fn send_wake(info: &WakeInfo) -> std::io::Result<usize> {
    let socket = tokio::net::UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))).await?;
    socket.set_broadcast(true)?;
    let mut targets: Vec<Ipv4Addr> = info.broadcasts.iter().filter_map(|b| b.parse().ok()).collect();
    targets.push(Ipv4Addr::BROADCAST);
    let mut sent = 0;
    for mac in info.macs.iter().filter_map(|m| parse_mac(m)) {
        let packet = magic_packet(mac);
        for ip in &targets {
            for port in [WOL_PORT, 7] {
                if socket.send_to(&packet, SocketAddrV4::new(*ip, port)).await.is_ok() {
                    sent += 1;
                }
            }
        }
    }
    Ok(sent)
}

/// Whether this computer runs on battery right now.
#[cfg(windows)]
pub fn on_battery() -> bool {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: the pointer is to a properly sized, writable struct.
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return false;
    }
    // 0: on battery; 1: plugged in; 255: unknown (desktops).
    status.ACLineStatus == 0
}

#[cfg(not(windows))]
pub fn on_battery() -> bool {
    false
}

/// Keeps this computer from going to sleep while `on` (the screen may still
/// turn off).
#[cfg(windows)]
pub fn keep_awake(on: bool) {
    use std::sync::Mutex;
    use std::sync::mpsc::{Sender, channel};
    use windows_sys::Win32::System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
    // The request belongs to the thread that made it: one thread holds it.
    static HOLDER: Mutex<Option<Sender<bool>>> = Mutex::new(None);
    let mut holder = HOLDER.lock().unwrap_or_else(|e| e.into_inner());
    if holder.is_none() {
        let (tx, rx) = channel::<bool>();
        let spawned = std::thread::Builder::new().name("nexus-keep-awake".into()).spawn(move || {
            let mut current = false;
            for on in rx {
                if on != current {
                    // SAFETY: plain flag call.
                    unsafe {
                        SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
                    }
                    current = on;
                }
            }
        });
        if spawned.is_err() {
            return;
        }
        *holder = Some(tx);
    }
    if let Some(tx) = holder.as_ref() {
        let _ = tx.send(on);
    }
}

#[cfg(not(windows))]
pub fn keep_awake(_on: bool) {}

/// While `on`, asks Windows 11 not to slow this service down: a program
/// without a window is otherwise put in Efficiency mode (EcoQoS), which
/// makes transfers crawl. When idle again, Windows decides as usual.
#[cfg(windows)]
pub fn full_speed(on: bool) {
    use std::sync::atomic::{AtomicU8, Ordering};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
    };
    // 0: not set yet, 1: Windows decides, 2: full speed.
    static CURRENT: AtomicU8 = AtomicU8::new(0);
    let wanted = if on { 2 } else { 1 };
    if CURRENT.swap(wanted, Ordering::Relaxed) == wanted {
        return;
    }
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        // Taking control of execution speed with the bit off means "never
        // throttle"; giving up control hands the choice back to Windows.
        ControlMask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
        StateMask: 0,
    };
    // SAFETY: a correctly sized, initialised struct for this information class.
    let ok = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    if ok == 0 {
        CURRENT.store(0, Ordering::Relaxed);
        tracing::debug!("could not change power throttling");
    }
}

#[cfg(not(windows))]
pub fn full_speed(_on: bool) {}

/// Puts this computer to sleep (not hibernation).
#[cfg(windows)]
pub fn sleep_now() {
    use windows_sys::Win32::System::Power::SetSuspendState;
    // SAFETY: plain call; false, false, false = sleep, not forced, wake events on.
    unsafe {
        SetSuspendState(false, false, false);
    }
}

#[cfg(not(windows))]
pub fn sleep_now() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_magic_packets() {
        let mac = parse_mac("AA-bb-cc-01-02-03").unwrap();
        let p = magic_packet(mac);
        assert_eq!(&p[..6], &[0xFF; 6]);
        assert_eq!(&p[6..12], &mac);
        assert_eq!(&p[96..102], &mac);
        assert!(parse_mac("aa:bb:cc").is_none());
        assert!(parse_mac("zz:bb:cc:01:02:03").is_none());
    }

    #[test]
    fn cleans_what_peers_send() {
        let info = WakeInfo {
            macs: vec!["AA:BB:CC:00:11:22".into(), "nope".into()],
            broadcasts: vec!["192.168.1.255".into(), "8.8.8.8".into(), "x".into()],
        }
        .cleaned();
        assert_eq!(info.macs, ["aa:bb:cc:00:11:22"]);
        assert_eq!(info.broadcasts, ["192.168.1.255"]);
    }

    /// Windows reports the throttling state this process asked for.
    #[cfg(windows)]
    #[test]
    fn full_speed_switches_efficiency_mode_off_and_back() {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, GetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling,
        };
        let read = || {
            let mut state = PROCESS_POWER_THROTTLING_STATE { Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION, ControlMask: 0, StateMask: 0 };
            // SAFETY: a correctly sized struct for this information class.
            let ok = unsafe {
                GetProcessInformation(
                    GetCurrentProcess(),
                    ProcessPowerThrottling,
                    (&mut state as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
                    size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                )
            };
            assert_ne!(ok, 0, "GetProcessInformation failed");
            (state.ControlMask, state.StateMask)
        };
        super::full_speed(true);
        assert_eq!(read(), (PROCESS_POWER_THROTTLING_EXECUTION_SPEED, 0));
        super::full_speed(false);
        assert_eq!(read(), (0, 0));
    }
}
