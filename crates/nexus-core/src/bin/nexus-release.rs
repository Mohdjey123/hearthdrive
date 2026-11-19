//! `nexus-release`: makes the update key and publishes signed updates.
//!
//! ```text
//! nexus-release keygen  [--key FILE]
//! nexus-release pubkey  [--key FILE]
//! nexus-release publish --platform P --version V --package FILE
//!                       [--name NAME] [--notes FILE] [--key FILE] [--data-dir DIR]
//! nexus-release sign    --platform P --version V --package FILE --out FILE
//!                       [--name NAME] [--notes FILE] [--key FILE] [--channel beta]
//! ```
//!
//! `publish` signs the package and adds it to a node's update store (this
//! computer's HearthDrive by default). From there it spreads to every paired
//! device. `sign` only writes the signed description to `--out`, to upload
//! next to the package on GitHub (named `nexus-update-<platform>.json`),
//! where every device also looks for updates. The key defaults to `NEXUS_UPDATE_KEY`, or
//! `~/.nexus-signing/update-key`. Anyone holding that file can publish
//! updates to every HearthDrive device that trusts it: keep it private and keep a
//! copy somewhere safe, because without it no further updates can be made.

use anyhow::{Context, bail};
use nexus_common::runtime::default_data_dir;
use nexus_devices::release::{self, PLATFORMS, Release, SignedRelease, TRUSTED_KEYS};
use std::path::PathBuf;

fn default_key_path() -> PathBuf {
    if let Some(path) = std::env::var_os("NEXUS_UPDATE_KEY") {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
    PathBuf::from(home).join(".nexus-signing").join("update-key")
}

fn read_key(path: &PathBuf) -> anyhow::Result<String> {
    let secret = std::fs::read_to_string(path).with_context(|| format!("reading the update key {}", path.display()))?;
    let secret = secret.trim().to_string();
    release::public_key_of(&secret).context("the update key file is not valid")?;
    Ok(secret)
}

struct Args {
    command: String,
    options: Vec<(String, String)>,
}

impl Args {
    fn parse() -> anyhow::Result<Self> {
        let mut args = std::env::args().skip(1);
        let command = args.next().unwrap_or_else(|| "help".into());
        let mut options = Vec::new();
        while let Some(flag) = args.next() {
            let Some(name) = flag.strip_prefix("--") else { bail!("unexpected argument: {flag}") };
            let value = args.next().with_context(|| format!("--{name} needs a value"))?;
            options.push((name.to_string(), value));
        }
        Ok(Self { command, options })
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.options.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn need(&self, name: &str) -> anyhow::Result<&str> {
        self.get(name).with_context(|| format!("--{name} is required"))
    }

    fn key_path(&self) -> PathBuf {
        self.get("key").map(PathBuf::from).unwrap_or_else(default_key_path)
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse()?;
    match args.command.as_str() {
        "keygen" => keygen(&args),
        "pubkey" => {
            let secret = read_key(&args.key_path())?;
            println!("{}", release::public_key_of(&secret).unwrap_or_default());
            Ok(())
        }
        "publish" => publish(&args),
        "sign" => {
            let out = PathBuf::from(args.need("out")?);
            let (release, signed, _) = sign(&args)?;
            std::fs::write(&out, serde_json::to_string_pretty(&signed)?).with_context(|| format!("writing {}", out.display()))?;
            println!("Signed {} {} ({} bytes) into {}", release.platform, release.version, release.size, out.display());
            Ok(())
        }
        _ => {
            println!("nexus-release keygen  [--key FILE]");
            println!("nexus-release pubkey  [--key FILE]");
            println!("nexus-release publish --platform P --version V --package FILE [--name NAME] [--notes FILE] [--key FILE] [--data-dir DIR]");
            println!("nexus-release sign    --platform P --version V --package FILE --out FILE [--name NAME] [--notes FILE] [--key FILE] [--channel beta]");
            println!("platforms: {}", PLATFORMS.join(", "));
            Ok(())
        }
    }
}

fn keygen(args: &Args) -> anyhow::Result<()> {
    let path = args.key_path();
    if path.exists() {
        bail!("{} already exists; it is never replaced. Remove it yourself if you really mean to.", path.display());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let (secret, public) = release::generate_key();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&path)?, format!("{secret}\n").as_bytes())?;
    println!("Update key written to {}", path.display());
    println!("Public key (put it in TRUSTED_KEYS): {public}");
    Ok(())
}

/// Hashes and signs the package the arguments name.
fn sign(args: &Args) -> anyhow::Result<(Release, SignedRelease, PathBuf)> {
    let platform = args.need("platform")?;
    if !PLATFORMS.contains(&platform) {
        bail!("unknown platform {platform}; use one of {}", PLATFORMS.join(", "));
    }
    let version = args.need("version")?;
    release::parse_version(version).context("the version must look like 1.2.3")?;
    let package = PathBuf::from(args.need("package")?);
    let notes = match args.get("notes") {
        Some(path) => std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?,
        None => String::new(),
    };
    let name = match args.get("name") {
        Some(name) => name.to_string(),
        None => package.file_name().context("the package has no file name")?.to_string_lossy().into_owned(),
    };

    let secret = read_key(&args.key_path())?;
    let public = release::public_key_of(&secret).unwrap_or_default();
    if !TRUSTED_KEYS.contains(&public.as_str()) {
        bail!("this update key is not built into the app (its public key is {public})");
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(std::fs::File::open(&package).with_context(|| format!("reading {}", package.display()))?)?;
    let release = Release {
        platform: platform.to_string(),
        version: version.to_string(),
        file: name,
        size: std::fs::metadata(&package)?.len(),
        blake3: hasher.finalize().to_hex().to_string(),
        notes: notes.trim().to_string(),
        published_at: nexus_common::time::now_ms(),
        channel: match args.get("channel") {
            None | Some("" | "stable") => String::new(),
            Some(release::BETA) => release::BETA.to_string(),
            Some(other) => bail!("unknown channel {other}; use stable or beta"),
        },
    };
    let signed = SignedRelease::sign(&release, &secret).context("signing failed")?;
    Ok((release, signed, package))
}

fn publish(args: &Args) -> anyhow::Result<()> {
    let (release, signed, package) = sign(args)?;
    let (platform, version) = (&release.platform, &release.version);
    let data_dir = args.get("data-dir").map(PathBuf::from).unwrap_or_else(default_data_dir);
    let keys: Vec<String> = TRUSTED_KEYS.iter().map(|k| k.to_string()).collect();
    nexus_api::update_store::import(&data_dir, &signed, &package, &keys)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("adding it to the update store in {}", data_dir.display()))?;
    println!("Published {platform} {version} ({} bytes) to {}", release.size, data_dir.display());
    Ok(())
}
