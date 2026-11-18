<div align="center">

# NEXUS Cloud

**Your devices. Your storage. One cloud.**

A private, self-hosted personal cloud made of your own Windows computers and Android phones.<br>
Free and open source. No subscription, no third-party storage, nothing exposed to the public internet.

![License: FSL-1.1-ALv2](https://img.shields.io/badge/license-FSL--1.1--ALv2-blue)
![Platforms: Windows | Android](https://img.shields.io/badge/platforms-Windows%20%7C%20Android-informational)
![Rust](https://img.shields.io/badge/backend-Rust%20%2B%20Axum-orange)
![Tauri 2](https://img.shields.io/badge/app-Tauri%202%20%2B%20React-24c8db)

</div>

---

Every computer you install NEXUS on becomes a storage node: it indexes the folders and drives you choose (in place, nothing is moved or uploaded anywhere) and serves them to your other devices. Devices pair with a link or a QR code and talk over end-to-end encrypted peer-to-peer connections, from any network, with nothing else to install.

> **Status:** Phases 0–6 are done: storage nodes, pairing, browsing and search across devices, copies, folder sync with versions, per-device and per-person access, encrypted share links, the Android app with photo backup, and updates that spread between your own devices. A store-signed Android build is next (see [Roadmap](#roadmap)).

## Contents

- [Features](#features)
- [What works today](#what-works-today)
- [Repository layout](#repository-layout)
- [Development](#development)
- [Connecting your computers](#connecting-your-computers)
- [Working across devices](#working-across-devices)
- [Sync and versions](#sync-and-versions)
- [Phone backup and the share menu](#phone-backup-and-the-share-menu)
- [Share links](#share-links)
- [Updates](#updates)
- [Windows installer](#windows-installer)
- [Android app](#android-app)
- [Roadmap](#roadmap)

## Features

- **Your files stay where they are.** Add existing folders or whole drives; NEXUS indexes them in place and never moves or copies them on its own.
- **One view of everything.** Browse, search, preview, stream, download and upload files on any of your devices from any other.
- **Built-in, encrypted networking.** Devices connect directly whenever possible and through an encrypted relay otherwise. No VPN, port forwarding or account with anyone.
- **Copy and sync between devices.** Background copies that resume after a dropped connection; one-way or two-way folder sync with conflict copies, a safety stop and version history.
- **Phone app.** The Android app runs the same node: browse your computers, back up photos and videos, and send files to a computer from any app's **Share** menu.
- **Share links.** Send anyone a link to a file or folder. With Send it is encrypted on your computer first, so the host never sees what it holds; links expire and can be limited to a number of downloads and protected with a password.
- **You decide who sees what.** Accounts with roles, and per-device and per-person limits to chosen storage locations, enforced on every device.
- **Updates from your own devices.** Publish a signed update on one computer and every paired computer and phone picks it up. Computers install it by themselves once their window is closed; phones ask first.
- **Built to be safe.** Loopback-only API, signed requests between devices, Argon2id passwords, path validation against traversal and link escapes, and an audit log.

## What works today

| Area | Implemented |
|---|---|
| Node | `nexus-service` background process that keeps running when the window closes; SQLite with migrations; daily rolling logs |
| Setup | First-run wizard: name the computer, create a local owner account (Argon2id), add folders or drives |
| Storage | Drive detection; add existing folders or whole drives (indexed in place, never moved); read-only roots; overlap and system-folder protection; removing a root never deletes files |
| Indexing | Fast metadata-only scan in 2,000-row batches; incremental rescans keep file ids stable; live filesystem watching; BLAKE3 hashes computed slowly in the background; drives that go offline keep their index and show as offline |
| Files | Unified browser (list and grid views), sort, multi-select, case-insensitive search across all storage, image/video/audio/text preview, HTTP range streaming, downloads, resumable uploads with progress and speed, new folder, rename, move, trash, restore, permanent delete, version history |
| Devices | Ed25519 identity per computer (private key encrypted with Windows DPAPI); pairing by link or QR code with a single-use 128-bit code, proof of key possession and owner approval after comparing fingerprints; per-device access role and storage limits; online/offline and latency monitoring; immediate removal on both sides |
| Networking | Built-in peer-to-peer networking ([iroh](https://iroh.computer)): QUIC connections dialed by device key, direct when possible (local network, NAT hole punching, UPnP), otherwise through an encrypted relay; public relays by default or your own; the HTTP API itself stays on loopback |
| Across devices | Browse, search, preview, stream, download, upload, rename, move and trash files on a paired device, relayed by your own node with signed requests; background copies of files and folders that resume after a dropped connection |
| Sync | One-way and two-way folder sync between devices; conflict copies; removals go to the trash; a safety stop for mass removals; the last ten versions of every replaced file |
| Share links | Links to a file or folder (sent as a zip) that anyone can open: end-to-end encrypted through [Send](https://github.com/timvisee/send) (AES-GCM, key only in the link) with expiry, a download limit and an optional password, or unencrypted through temp.sh for larger files; take a link down at any time; uploads appear under Transfers |
| Updates | Signed packages (Ed25519 over the version, size and BLAKE3 hash) passed between paired devices; computers keep packages for every platform to pass on; Windows installs by itself when its window is closed and nothing is copying, Android opens the system installer |
| People | Accounts with roles (owner, administrator, editor, viewer); per-person storage limits that also apply on paired devices |
| Android | The same node inside the app; QR pairing; photo and video backup to a computer; NEXUS in the system share menu |
| Security | Loopback-only API by default; per-run local token for the desktop app; sessions stored as token hashes; short-lived signed URLs for media; all paths validated against their root (traversal, reserved names, ADS, symlink and junction escapes); DNS-rebinding Host check; a strict Content Security Policy on the web interface; user content and everything shown from a paired device served with `CSP: sandbox`; origin check on the live event stream; per-person limits enforced on downloads too; phones accept first-run setup only from the app itself and keep their data out of Android backups; audit log |

## Repository layout

```
apps/desktop/          React + TypeScript UI (Vite, Tailwind) and the Tauri 2 shell (src-tauri/)
crates/
  nexus-common/        Shared types, errors, events, runtime discovery
  nexus-database/      SQLite access (SQLx); one database per node
  nexus-storage/       Storage roots, safe path resolution, file operations, drive detection
  nexus-indexer/       Scanner, filesystem watcher, background hasher, availability monitor
  nexus-security/      Argon2id, tokens, signed tickets, role checks
  nexus-network/       Peer-to-peer endpoint (iroh): device-key addressing, relays, connection cache
  nexus-devices/       Device identity keys (DPAPI), signed peer requests, replay guard, pairing links
  nexus-api/           Axum REST + WebSocket API under /api/v1
  nexus-core/          Node lifecycle and the `nexus-service` binary
migrations/            SQL migrations (embedded at build time)
scripts/               Android build script
docs/                  Architecture and development notes
```

## Development

Prerequisites: Rust (stable), Node.js 20+, and the [Tauri Windows prerequisites](https://v2.tauri.app/start/prerequisites/) (WebView2 ships with Windows 11). End users need none of these.

```bash
npm install
```

Run the desktop app. This builds the service and starts Tauri with hot reload; the shell starts `nexus-service` automatically:

```bash
npm run desktop
```

Or run just the service and use a browser. The service serves the built UI at http://127.0.0.1:47100:

```bash
npm run build:ui
```

```bash
npm run service
```

Tests:

```bash
cargo test
```

Service options: `--data-dir`, `--port`, `--ui-dir` (or `NEXUS_DATA_DIR`, `NEXUS_PORT`, `NEXUS_UI_DIR`). Data defaults to `%LOCALAPPDATA%\NEXUS Cloud`. It holds the database, logs, `runtime.json` and the encrypted device key (`identity.key`), never your files.

Networking options:

| Variable | Effect |
| --- | --- |
| `NEXUS_RELAY` | `default` (public relays run by the iroh project), `off` (direct connections only), or a comma-separated list of your own relay URLs |
| `NEXUS_P2P_PORT` | UDP port for peer-to-peer traffic (defaults to the API port, 47100; a random port is used if it is taken) |
| `NEXUS_NETWORK=local` | Development: the endpoint binds loopback only with no relay, so only nodes on this computer can connect (two nodes with different `--port` and `--data-dir` can pair) |
| `NEXUS_RELAY_ONLY=1` | Diagnostics: never use direct paths, to see what a device on a far-away network experiences |

## Connecting your computers

Nothing extra to install: each NEXUS node connects to other nodes by itself.

1. Open **Devices** in NEXUS on the first computer and choose **Add device**. **Connections** should say **Reachable from anywhere**.
2. On the second computer, choose **Join a cloud** (or **Join an existing cloud** during setup) and paste the link.
3. Back on the first computer, check that the fingerprint matches the one shown on the second, and approve.

Both computers can then browse each other from **Files** → device menu, from any network. Each keeps its own files and database. Each device card shows whether the connection is **Direct** or **Relayed**.

**Your other devices pair by themselves.** Pair each new device with one computer (your main node) and that computer introduces it to every other device it is paired with, and them to it, within about a minute. An introduced device is marked **Introduced by …** and is trusted no further than the device that introduced it: its role there is the lesser of the two, it sees only the storage the introducer sees, and it never introduces others in turn. Unpairing a device on the main node removes it from the others too; a device you remove by hand is not added again. Turn this off with **Devices** → **Pair my devices with each other** (this also removes the devices it added).

How it works: every connection is QUIC with TLS 1.3, authenticated with the two devices' own keys. Devices first try to connect directly. On the same network that always works; across the internet it usually works through NAT hole punching. When it doesn't (some corporate, hotel or mobile networks), traffic goes through a relay server. A relay only forwards encrypted packets and cannot read them. The free public relays are rate-limited, so large transfers over a relay are slow. If you often hit that, run your own [`iroh-relay`](https://docs.iroh.computer/relays) on a small server and start NEXUS with `NEXUS_RELAY=https://relay.example.com`. The first time NEXUS runs, Windows may ask whether to allow `nexus-service` through the firewall. Allow it on private networks; it only accepts connections from paired devices and pairing requests that carry a valid code.

## Working across devices

* **No waiting for a paired device.** Storage, folders and the device list of paired devices are remembered on screen, also after restarting the app, and refreshed in the background, so switching tabs shows them at once. They are forgotten when you sign out.
* **Search everywhere.** In **Files**, type a search and tick **All devices** to search this computer and every paired device that is online. Results say which device each file is on.
* **Copy between devices.** Select one or more files and choose **Copy to another device**. Pick the device, storage and folder. Folders are copied with everything inside them. The copy runs in the background (see **Transfers**); the content streams from the source to the destination through your node, nothing is stored in between, and a dropped connection continues from where it stopped.
* **Limit what a device sees.** On **Devices**, each paired device shows **Storage it can see here**. Choose all storage or specific locations. Everything else is invisible to that device: it cannot list, search, download or upload there.
* **People.** Under **Settings** → **People**, an owner or administrator can add accounts (administrator, editor or viewer), change roles and remove people. People sign in from a browser on the computer itself. The only owner cannot be removed or demoted.
* **Limit what a person sees.** Next to each editor or viewer, the storage button opens **What … can use**: all storage, or chosen locations on this computer and on your paired devices. Everything else is invisible to them, on every device, including in search, sync, transfers and the activity log. Owners and administrators always see everything. A change applies to their very next request.
* **Start with Windows.** **Settings** → **Start with Windows** registers the storage service to start when you sign in (a per-user start-up entry; no administrator rights).

## Sync and versions

* **Sync a folder.** Open **Sync** → **New sync**, choose a folder here and a folder on a paired computer, and a direction: both ways, only to the device, or only from the device. Sync runs within about 20 seconds of a change here, every five minutes otherwise, and whenever you press **Sync now**.
* **Nothing is deleted outright.** A file removed on one side goes to the trash on the other. If a file changed on both sides, the newer one keeps its name and the other is kept beside it as `name (Device's copy date).ext`.
* **Safety stop.** If most of a folder disappears at once (an unplugged drive, a moved folder), sync does not remove the files on the other side. It says so on the Sync page; press **Sync now** if the removal was meant.
* **Earlier versions.** When sync, a copy or an upload with "replace" overwrites a file, the old content is kept (the last ten per file). Open **Properties** on a file to download or restore one. Restoring keeps the current content as a version too.
* **Resumable uploads.** Files over 8 MB are uploaded in pieces. If the connection drops, the upload continues where it stopped; choosing **Retry** (even after closing the app) picks up from there.

## Phone backup and the share menu

* **Back up photos and videos.** In the phone app, open **Backup**, choose what to back up (camera and screenshots, pictures, videos), a paired computer and a folder on it, and allow NEXUS to read your photos when Android asks. Photos keep their folders and dates (for example `DCIM/Camera/IMG_1234.jpg`).
* **When it runs.** While NEXUS is open on the phone: when you open it, every 15 minutes, and when you press **Back up now**. Only new and changed files are sent; a photo edited on the phone replaces its copy, and the computer keeps the earlier one as a version.
* **Nothing is removed.** Deleting a photo on the phone keeps the copy on the computer. Deleting the copy on the computer does not make the phone send it again. Files already on the computer with the same name and size (after reinstalling the app, say) are not sent twice.
* **Share to NEXUS.** In any app, choose **Share** → **NEXUS** for files, photos or text. NEXUS asks which computer and folder to send them to; the transfer appears under **Transfers**. **Later** keeps them in the app until you send or discard them.

## Share links

* **Make a link.** In **Files**, select a file or folder and choose **Share a link**. Pick how long it works (up to 3 days on the public Send service) and how many downloads it allows, and optionally a password. NEXUS uploads it in the background; the link appears in the dialog and under **Shares**, ready to copy. A folder is sent as one zip file.
* **Encrypted with Send.** The file is encrypted on the computer that holds it, with a new random key for every link. The key is the part of the link after `#`, which browsers never send to a server, so the Send service stores only data it cannot read. The recipient's browser decrypts it. A password is never stored by NEXUS; it is turned into the key the service checks before a download.
* **Which Send service.** By default the public instance at send.vis.ee (files up to 2.5 GB). An owner or administrator can point NEXUS at another Send instance, including one you host, under **Shares** → **Send service**.
* **temp.sh.** For larger files (up to 4 GB, 3 days) NEXUS can use temp.sh instead. It is **not encrypted**: the service can read what you upload, and the link cannot be taken down early. Use it only for files that are not private.
* **Take a link down.** On **Shares**, the bin deletes the file from Send at once. Links stop working by themselves when they expire or run out of downloads.
* **On the phone.** Share files on your computers the same way; the computer holding the file does the encrypting and uploading, and the phone offers its share sheet to send the link.
* **Who can share.** Owners, administrators and editors. Owners and administrators see every link made on a computer; everyone else sees their own. People limited to some storage can share only from it.

## Updates

Updates do not come from a download server. They are published on one of your computers and spread between your own devices over the same encrypted connections they already use.

* **Publishing.** `scripts/release.sh 0.1.3 notes.md` sets the version, builds the Windows installer and the Android APK, signs both with the update key and adds them to this computer's NEXUS. `--github` also creates a GitHub release. The first time, make the key with `target/release/nexus-release keygen` and put its public key in `TRUSTED_KEYS` (`crates/nexus-devices/src/release.rs`).
* **Spreading.** Every device asks its online paired devices what they hold every 30 minutes (or at once with **Settings** → **Updates** → **Check now**) and fetches anything newer. Computers keep the packages for every platform so they can pass them on; phones keep only their own.
* **Checking.** A package is kept, offered or installed only if it is signed with a key built into the app and matches the signed size and BLAKE3 hash, checked again right before installing. A paired device can hold an update back, but it cannot change one or roll a device back to an older version.
* **Installing on Windows.** A banner offers **Install now**. With **Install updates automatically** on (the default), the update installs by itself once the window is closed and no copy or sync is running. The installer stops the service, replaces the files and starts the service again. MSI installs and development builds are not updated this way.
* **Installing on Android.** The app shows the same banner; **Install now** opens Android's installer, which asks you to confirm (the first time, Android asks you to allow NEXUS to install apps). Updates install over the existing app only when they are signed with the same Android key, so build releases on the same computer.
* **The update key** is at `~/.nexus-signing/update-key` (or `NEXUS_UPDATE_KEY`). Anyone with that file can publish updates to all your devices, so keep it private. Keep a copy somewhere safe too: without it no further updates can be made, and devices would have to be updated by hand with a build that trusts a new key.

Versions before 0.1.2 cannot update themselves: install 0.1.2 by hand once on each device.

## Windows installer

```bash
npm run desktop:build
```

builds the service and the app in release mode and produces an NSIS installer (`.exe`) under `target/release/bundle/nsis/` and an MSI under `target/release/bundle/msi/`. Ready-made builds are attached to the [GitHub releases](../../releases). The installer puts `nexus-service.exe` and the web interface next to the app. It is not code-signed, so Windows SmartScreen will warn on first run.

## Android app

The Android app is the same node and the same interface, packaged with Tauri. The node runs inside the app, so the phone pairs and connects exactly like a computer does. A phone holds no storage of its own: it opens on the files of the computers it is paired with.

1. Build the APK (Android SDK and NDK, a JDK, and the Rust Android targets are required):

   ```bash
   scripts/build-android.sh aarch64 release
   ```

   `x86_64 debug` builds for the emulator. The script prints the APK's path. Release builds are signed with the local Android debug key so they install directly; sign with your own key before publishing.
2. Install it (`adb install -r <apk>`, or copy the file to the phone and open it).
3. Open the app, name the phone, and choose **Scan QR code**. On the computer, open **Devices** → **Add device** and approve the phone after comparing fingerprints.

Tap opens a file or folder; press and hold selects. Downloads are saved to the phone's `Download/NEXUS` folder. The node runs while the app is open; the computers keep running on their own. Phone backup and sharing to NEXUS are described above.

The script exists because the Tauri CLI links the compiled library with a symbolic link, which Windows only allows in Developer Mode. The script copies the library instead.

## Roadmap

Following the product spec's phases. Each phase ships a working application.

- [x] **Phase 0 — Foundation:** workspace, service, Tauri shell, React UI, migrations, logging
- [x] **Phase 1 — Local storage:** drives, roots, indexing, browser, search, upload/download, previews
- [x] **Phase 2 — Networking:** built-in peer-to-peer connections (no VPN client), device identity keys, link/QR pairing, signed requests, online/offline peers, remote browsing
- [x] **Phase 3 — Android:** native app with the node built in, QR pairing, remote browsing, previews, downloads, uploads
- [x] **Phase 4 — Multi-node storage:** search across all devices, copying files and folders between devices, resumable uploads and copies
- [x] **Phase 5 — Synchronization:** one-way and two-way folder sync, conflict copies, version history (no real-time change journal yet: devices are checked every five minutes)
- [x] **Phase 6 — Sharing and security:** accounts with roles, per-device and per-person storage access, end-to-end encrypted share links
- [ ] **Phase 7 — Release:** Windows installer, start-with-Windows and updates between your devices are done; a store-signed Android release is not

## Documentation

- [Architecture overview](docs/architecture/overview.md): processes, data model, indexing, path safety, networking, sync, access and the Android app.

## License

FSL-1.1-ALv2: the Functional Source License, version 1.1, with Apache 2.0 as the future license. See [LICENSE.md](LICENSE.md).
