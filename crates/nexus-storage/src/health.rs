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
    parse_report(&out)
}

#[cfg(not(windows))]
pub fn physical_disks() -> Result<Vec<PhysicalDisk>, String> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

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
