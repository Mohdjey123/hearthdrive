//! What Windows reports about the physical disks: health status, and where
//! it allows it (usually only to administrators), temperature, wear and
//! uncorrected errors.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PhysicalDisk {
    /// Windows' disk number.
    pub number: String,
    pub model: String,
    /// `hdd`, `ssd` or `unknown`.
    pub media: String,
    /// `SATA`, `NVMe`, `USB`, …
    pub bus: String,
    /// `healthy`, `warning`, `unhealthy` or `unknown`.
    pub health: String,
    /// Windows' operational status, e.g. "OK" or "Predictive Failure".
    pub status: String,
    pub size: u64,
    /// Drive letters on this disk, e.g. `["C"]`.
    pub letters: Vec<String>,
    pub temperature_c: Option<u32>,
    pub temperature_max_c: Option<u32>,
    /// Percent of rated life used (SSDs).
    pub wear_percent: Option<u32>,
    pub power_on_hours: Option<u64>,
    pub read_errors: Option<u64>,
    pub write_errors: Option<u64>,
    /// Whether temperature, wear and error counts could be read at all.
    pub details: bool,
}

#[derive(Deserialize)]
struct RawReport {
    #[serde(default)]
    disks: Vec<RawDisk>,
    #[serde(default)]
    parts: Vec<RawPart>,
}

#[derive(Deserialize)]
struct RawDisk {
    id: Option<String>,
    model: Option<String>,
    media: Option<String>,
    bus: Option<String>,
    health: Option<String>,
    status: Option<String>,
    size: Option<u64>,
    #[serde(default)]
    details: bool,
    temp: Option<u32>,
    temp_max: Option<u32>,
    wear: Option<u32>,
    hours: Option<u64>,
    read_errors: Option<u64>,
    write_errors: Option<u64>,
}

#[derive(Deserialize)]
struct RawPart {
    d: Option<String>,
    l: Option<String>,
}

/// Parses the report the PowerShell script prints.
pub fn parse_report(json: &str) -> Result<Vec<PhysicalDisk>, String> {
    let raw: RawReport = serde_json::from_str(json.trim()).map_err(|e| format!("unreadable disk report: {e}"))?;
    let mut disks: Vec<PhysicalDisk> = raw
        .disks
        .into_iter()
        .map(|d| {
            let number = d.id.unwrap_or_default();
            let mut letters: Vec<String> = raw
                .parts
                .iter()
                .filter(|p| p.d.as_deref() == Some(number.as_str()))
                .filter_map(|p| p.l.clone())
                .filter(|l| l.len() == 1 && l.chars().all(|c| c.is_ascii_alphabetic()))
                .map(|l| l.to_ascii_uppercase())
                .collect();
            letters.sort();
            letters.dedup();
            // Reliability counters report 0 for "not known".
            let known = |v: Option<u32>| v.filter(|t| *t > 0);
            PhysicalDisk {
                model: d.model.map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).unwrap_or_else(|| format!("Disk {number}")),
                number,
                media: match d.media.as_deref().map(str::to_ascii_lowercase).as_deref() {
                    Some("hdd") => "hdd",
                    Some("ssd") => "ssd",
                    _ => "unknown",
                }
                .into(),
                bus: d.bus.unwrap_or_default(),
                health: match d.health.as_deref().map(str::to_ascii_lowercase).as_deref() {
                    Some("healthy") => "healthy",
                    Some("warning") => "warning",
                    Some("unhealthy") => "unhealthy",
                    _ => "unknown",
                }
                .into(),
                status: d.status.unwrap_or_default(),
                size: d.size.unwrap_or(0),
                letters,
                temperature_c: known(d.temp),
                temperature_max_c: known(d.temp_max),
                wear_percent: if d.details { d.wear } else { None },
                power_on_hours: d.hours.filter(|h| *h > 0),
                read_errors: if d.details { d.read_errors } else { None },
                write_errors: if d.details { d.write_errors } else { None },
                details: d.details,
            }
        })
        .collect();
    disks.sort_by(|a, b| a.letters.first().cmp(&b.letters.first()).then_with(|| a.number.cmp(&b.number)));
    Ok(disks)
}

/// Lists the physical disks this computer reports health for, as the
/// script below sees them. May take a few seconds; call it off the async
/// runtime. Empty where Windows' storage tools are not available.
#[cfg(windows)]
pub fn physical_disks() -> Result<Vec<PhysicalDisk>, String> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$parts = @(Get-Partition | Where-Object DriveLetter | ForEach-Object { @{ d = [string]$_.DiskNumber; l = [string]$_.DriveLetter } })
$disks = @(foreach ($p in Get-PhysicalDisk) {
  $r = $null
  try { $r = $p | Get-StorageReliabilityCounter -ErrorAction Stop } catch {}
  @{
    id = [string]$p.DeviceId; model = [string]$p.FriendlyName; media = [string]$p.MediaType; bus = [string]$p.BusType
    health = [string]$p.HealthStatus; status = (@($p.OperationalStatus) -join ', '); size = [uint64]$p.Size
    details = ($null -ne $r)
    temp = $r.Temperature; temp_max = $r.TemperatureMax; wear = $r.Wear; hours = $r.PowerOnHours
    read_errors = $r.ReadErrorsUncorrected; write_errors = $r.WriteErrorsUncorrected
  }
})
@{ disks = $disks; parts = $parts } | ConvertTo-Json -Depth 4 -Compress
"#;
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", SCRIPT])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("could not ask Windows about the disks: {e}"))?;
    // A failing disk can make Windows' storage tools hang; never wait forever.
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > Duration::from_secs(45) => {
                let _ = child.kill();
                return Err("Windows took too long to report on the disks".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_string(&mut out).map_err(|e| e.to_string())?;
    }
    if out.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut disks = parse_report(&out)?;
    // Without administrator rights Windows keeps the reliability counters
    // to itself; ask the disks directly instead.
    for d in disks.iter_mut().filter(|d| !d.details) {
        if let Ok(n) = d.number.parse::<u32>() {
            fill(d, &read_direct(n));
        }
    }
    Ok(disks)
}

#[cfg(not(windows))]
pub fn physical_disks() -> Result<Vec<PhysicalDisk>, String> {
    Ok(Vec::new())
}


/// What a disk tells about itself through storage requests any program may
/// send (no administrator needed): temperature, failure prediction and,
/// for NVMe drives, the health log.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DirectHealth {
    pub temperature_c: Option<u32>,
    pub critical_c: Option<u32>,
    pub predict_failure: Option<bool>,
    /// NVMe critical warning bits (0: none).
    pub critical_warning: Option<u8>,
    pub spare_percent: Option<u32>,
    pub used_percent: Option<u32>,
    pub power_on_hours: Option<u64>,
    pub media_errors: Option<u64>,
}

/// Reads the NVMe "SMART / health information" log page.
pub fn parse_nvme_health(log: &[u8]) -> DirectHealth {
    if log.len() < 192 {
        return DirectHealth::default();
    }
    let u128_at = |at: usize| u128::from_le_bytes(log[at..at + 16].try_into().unwrap());
    let kelvin = u16::from_le_bytes([log[1], log[2]]) as i32;
    DirectHealth {
        temperature_c: (kelvin > 200).then(|| (kelvin - 273).max(0) as u32),
        critical_warning: Some(log[0]),
        spare_percent: Some(log[3] as u32),
        used_percent: Some(log[5] as u32),
        power_on_hours: Some(u128_at(128).min(u64::MAX as u128) as u64),
        media_errors: Some(u128_at(160).min(u64::MAX as u128) as u64),
        ..Default::default()
    }
}

/// Adds what the disk said to what Windows reported.
pub fn fill(d: &mut PhysicalDisk, h: &DirectHealth) {
    let mut any = false;
    if d.temperature_c.is_none()
        && let Some(t) = h.temperature_c.filter(|t| (1..150).contains(t))
    {
        d.temperature_c = Some(t);
        any = true;
    }
    if d.temperature_max_c.is_none()
        && let Some(t) = h.critical_c.filter(|t| (1..150).contains(t))
    {
        d.temperature_max_c = Some(t);
    }
    if let Some(w) = h.used_percent {
        d.wear_percent = Some(w);
        any = true;
    }
    if let Some(hours) = h.power_on_hours.filter(|h| *h > 0) {
        d.power_on_hours = Some(hours);
        any = true;
    }
    if let Some(e) = h.media_errors {
        d.read_errors = Some(e);
        any = true;
    }
    let note = |d: &mut PhysicalDisk, text: &str| {
        d.status = if d.status.is_empty() { text.to_string() } else { format!("{}; {text}", d.status) };
    };
    if h.critical_warning.is_some_and(|w| w != 0) {
        if d.health == "healthy" || d.health == "unknown" {
            d.health = "warning".into();
        }
        note(d, "The drive reports a critical warning");
    }
    if h.predict_failure == Some(true) {
        d.health = "unhealthy".into();
        note(d, "The drive predicts it will fail");
        any = true;
    }
    d.details |= any;
}

#[cfg(windows)]
mod direct {
    use super::DirectHealth;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_STORAGE_PREDICT_FAILURE, IOCTL_STORAGE_QUERY_PROPERTY, NVMeDataTypeLogPage, PropertyStandardQuery, ProtocolTypeNvme,
        StorageDeviceProtocolSpecificProperty, StorageDeviceTemperatureProperty,
    };

    struct Disk(HANDLE);

    impl Drop for Disk {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    impl Disk {
        /// Opened for questions only (no reading or writing), which needs
        /// no administrator.
        fn open(n: u32) -> Option<Disk> {
            let name: Vec<u16> = format!(r"\\.\PhysicalDrive{n}").encode_utf16().chain([0]).collect();
            let h = unsafe {
                CreateFileW(name.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut())
            };
            (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(Disk(h))
        }

        fn ask(&self, code: u32, input: &[u8], out_len: usize) -> Option<Vec<u8>> {
            let mut out = vec![0u8; out_len];
            let mut got = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    self.0,
                    code,
                    if input.is_empty() { std::ptr::null() } else { input.as_ptr().cast() },
                    input.len() as u32,
                    out.as_mut_ptr().cast(),
                    out.len() as u32,
                    &mut got,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return None;
            }
            out.truncate(got as usize);
            Some(out)
        }
    }

    fn i16_at(b: &[u8], at: usize) -> Option<i16> {
        b.get(at..at + 2).map(|x| i16::from_le_bytes([x[0], x[1]]))
    }

    fn u32_at(b: &[u8], at: usize) -> Option<u32> {
        b.get(at..at + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
    }

    pub fn read(n: u32) -> DirectHealth {
        let Some(disk) = Disk::open(n) else { return DirectHealth::default() };
        let mut h = DirectHealth::default();

        // NVMe health log: a STORAGE_PROPERTY_QUERY, then the protocol request.
        const PROTO: usize = 40;
        const LOG: usize = 512;
        let mut q = vec![0u8; 8 + PROTO + LOG];
        q[0..4].copy_from_slice(&StorageDeviceProtocolSpecificProperty.to_le_bytes());
        q[4..8].copy_from_slice(&PropertyStandardQuery.to_le_bytes());
        let fields: [u32; 6] = [ProtocolTypeNvme as u32, NVMeDataTypeLogPage as u32, 2, 0, PROTO as u32, LOG as u32];
        for (i, v) in fields.iter().enumerate() {
            q[8 + i * 4..12 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        if let Some(out) = disk.ask(IOCTL_STORAGE_QUERY_PROPERTY, &q, q.len())
            && let Some(offset) = u32_at(&out, 8 + 16)
            && let Some(log) = out.get(8 + offset as usize..8 + offset as usize + LOG)
        {
            h = super::parse_nvme_health(log);
        }

        // Temperature, for any kind of drive that reports it.
        let mut q = vec![0u8; 12];
        q[0..4].copy_from_slice(&StorageDeviceTemperatureProperty.to_le_bytes());
        q[4..8].copy_from_slice(&PropertyStandardQuery.to_le_bytes());
        if let Some(out) = disk.ask(IOCTL_STORAGE_QUERY_PROPERTY, &q, 1024) {
            let count = out.get(12..14).map(|x| u16::from_le_bytes([x[0], x[1]])).unwrap_or(0);
            if count > 0
                && h.temperature_c.is_none()
                && let Some(t) = i16_at(&out, 26).filter(|t| *t > 0)
            {
                h.temperature_c = Some(t as u32);
            }
            h.critical_c = i16_at(&out, 8).filter(|t| *t > 0).map(|t| t as u32);
        }

        // SMART's own verdict (ATA drives).
        if let Some(out) = disk.ask(IOCTL_STORAGE_PREDICT_FAILURE, &[], 516) {
            h.predict_failure = u32_at(&out, 0).map(|v| v != 0);
        }
        h
    }
}

#[cfg(not(windows))]
mod direct {
    pub fn read(_: u32) -> super::DirectHealth {
        super::DirectHealth::default()
    }
}

/// Asks a disk directly (Windows' disk number).
pub fn read_direct(n: u32) -> DirectHealth {
    direct::read(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_nvme_health_log() {
        let mut log = vec![0u8; 512];
        log[1..3].copy_from_slice(&(273u16 + 45).to_le_bytes());
        log[3] = 100;
        log[5] = 7;
        log[128] = 0x10;
        log[129] = 0x27; // 10000 hours
        log[160] = 2;
        let h = parse_nvme_health(&log);
        assert_eq!((h.temperature_c, h.used_percent, h.power_on_hours, h.media_errors), (Some(45), Some(7), Some(10_000), Some(2)));
        let mut d = PhysicalDisk {
            number: "0".into(), model: "x".into(), media: "ssd".into(), bus: "NVMe".into(), health: "healthy".into(),
            status: "OK".into(), size: 1, letters: vec![], temperature_c: None, temperature_max_c: None, wear_percent: None,
            power_on_hours: None, read_errors: None, write_errors: None, details: false,
        };
        fill(&mut d, &DirectHealth { critical_warning: Some(4), ..h });
        assert!(d.details);
        assert_eq!((d.health.as_str(), d.wear_percent), ("warning", Some(7)));
    }

    /// Runs against this computer's disks and prints what they said.
    #[test]
    #[ignore = "asks this computer's disks"]
    fn asks_this_computers_disks() {
        for n in 0..4 {
            println!("disk {n}: {:?}", read_direct(n));
        }
    }

    #[test]
    fn reads_a_report() {
        let json = r#"{"parts":[{"d":"1","l":"C"},{"d":"0","l":"D"},{"d":"2","l":"G"}],
          "disks":[
            {"id":"0","model":"TOSHIBA HDWR440","media":"HDD","bus":"SATA","health":"Healthy","status":"OK","size":4000787030016,"details":false,"temp":null},
            {"id":"1","model":"WD Blue SN570 1TB","media":"SSD","bus":"NVMe","health":"Warning","status":"Degraded","size":1000204886016,"details":true,"temp":41,"temp_max":0,"wear":3,"hours":5100,"read_errors":0,"write_errors":0},
            {"id":"2","model":"  ","media":"Unspecified","bus":"USB","health":"Healthy","status":"OK","size":31004295168}
          ]}"#;
        let disks = parse_report(json).unwrap();
        assert_eq!(disks.iter().map(|d| d.letters.join("")).collect::<Vec<_>>(), ["C", "D", "G"]);
        let ssd = &disks[0];
        assert_eq!((ssd.media.as_str(), ssd.health.as_str()), ("ssd", "warning"));
        assert_eq!((ssd.temperature_c, ssd.temperature_max_c, ssd.wear_percent, ssd.power_on_hours), (Some(41), None, Some(3), Some(5100)));
        assert_eq!(ssd.read_errors, Some(0));
        let hdd = &disks[1];
        assert_eq!((hdd.media.as_str(), hdd.temperature_c, hdd.details, hdd.read_errors), ("hdd", None, false, None));
        assert_eq!((disks[2].model.as_str(), disks[2].media.as_str()), ("Disk 2", "unknown"));
    }

    #[test]
    fn empty_and_broken_reports() {
        assert_eq!(parse_report("{}").unwrap(), Vec::new());
        assert!(parse_report("not json").is_err());
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore = "asks Windows about this computer's disks"]
    fn this_computer() {
        let t = std::time::Instant::now();
        let disks = super::physical_disks().unwrap();
        eprintln!("{:#?}\n{:?}", disks, t.elapsed());
        assert!(!disks.is_empty());
    }
}
