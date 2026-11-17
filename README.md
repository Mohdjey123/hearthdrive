# NEXUS Cloud

**Your devices. Your storage. One cloud.**

NEXUS Cloud turns your own computers into a private, self-hosted personal cloud. Free and open source, with no subscription and no third-party storage.

> **Status: Phases 0–3.** Each Windows computer is a complete cloud node (indexing, browsing, search, upload, download, preview, rename, move, trash, restore). Computers and Android phones pair with a link or QR code, then browse, preview, download and upload each other's files from any network, with built-in encrypted peer-to-peer connections and nothing else to install. Sync and sharing come next (see [Roadmap](#roadmap)).

## What works today

| Area | Implemented |
|---|---|
| Node | `nexus-service` background process that keeps running when the window closes; SQLite with migrations; daily rolling logs |
| Setup | First-run wizard: name the computer, create a local owner account (Argon2id), add folders or drives |
| Storage | Drive detection; add existing folders or whole drives (indexed in place, never moved); read-only roots; overlap and system-folder protection; removing a root never deletes files |
| Indexing | Fast metadata-only scan in 2,000-row batches; incremental rescans keep file ids stable; live filesystem watching; BLAKE3 hashes computed slowly in the background; drives that go offline keep their index and show as offline |
| Files | Unified browser (list and grid views), sort, multi-select, case-insensitive search across all storage, image/video/audio/text preview, HTTP range streaming, downloads, uploads with progress and speed, new folder, rename, move, trash, restore, permanent delete |
| Devices | Ed25519 identity per computer (private key encrypted with Windows DPAPI); pairing by link or QR code with a single-use 128-bit code, proof of key possession and owner approval after comparing fingerprints; per-device access role; online/offline and latency monitoring; immediate removal on both sides |
| Networking | Built-in peer-to-peer networking ([iroh](https://iroh.computer)): QUIC connections dialed by device key, direct when possible (local network, NAT hole punching, UPnP), otherwise through an encrypted relay; public relays by default or your own; the HTTP API itself stays on loopback |
| Remote browsing | Browse, search, preview, stream, download, upload, rename, move and trash files on a paired computer, relayed by your own node with signed requests; media uses the remote node's own short-lived tickets |
| Security | Loopback-only API by default; per-run local token for the desktop app; sessions stored as token hashes; short-lived signed URLs for media; all paths validated against their root (traversal, reserved names, ADS, symlink and junction escapes); uploads never overwrite files; DNS-rebinding Host check; user content served with `CSP: sandbox`; audit log |

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

How it works: every connection is QUIC with TLS 1.3, authenticated with the two devices' own keys. Devices first try to connect directly. On the same network that always works; across the internet it usually works through NAT hole punching. When it doesn't (some corporate, hotel or mobile networks), traffic goes through a relay server. A relay only forwards encrypted packets and cannot read them. The free public relays are rate-limited, so large transfers over a relay are slow. If you often hit that, run your own [`iroh-relay`](https://docs.iroh.computer/relays) on a small server and start NEXUS with `NEXUS_RELAY=https://relay.example.com`. The first time NEXUS runs, Windows may ask whether to allow `nexus-service` through the firewall. Allow it on private networks; it only accepts connections from paired devices and pairing requests that carry a valid code.

## Android app

The Android app is the same node and the same interface, packaged with Tauri. The node runs inside the app, so the phone pairs and connects exactly like a computer does. A phone holds no storage of its own: it opens on the files of the computers it is paired with.

1. Build the APK (Android SDK and NDK, a JDK, and the Rust Android targets are required):

   ```bash
   scripts/build-android.sh aarch64 release
   ```

   `x86_64 debug` builds for the emulator. The script prints the APK's path. Release builds are signed with the local Android debug key so they install directly; sign with your own key before publishing.
2. Install it (`adb install -r <apk>`, or copy the file to the phone and open it).
3. Open the app, name the phone, and choose **Scan QR code**. On the computer, open **Devices** → **Add device** and approve the phone after comparing fingerprints.

Tap opens a file or folder; press and hold selects. Downloads are saved to the phone's `Download/NEXUS` folder. The node runs while the app is open; the computers keep running on their own.

The script exists because the Tauri CLI links the compiled library with a symbolic link, which Windows only allows in Developer Mode. The script copies the library instead.

## Roadmap

Following the product spec's phases. Each phase ships a working application.

- [x] **Phase 0 — Foundation:** workspace, service, Tauri shell, React UI, migrations, logging
- [x] **Phase 1 — Local storage:** drives, roots, indexing, browser, search, upload/download, previews
- [x] **Phase 2 — Networking:** built-in peer-to-peer connections (no VPN client), device identity keys, link/QR pairing, signed requests, online/offline peers, remote browsing
- [x] **Phase 3 — Android:** native app with the node built in, QR pairing, remote browsing, previews, downloads, uploads
- [ ] **Phase 4 — Multi-node storage:** unified index across nodes, remote operations, resumable chunked transfers
- [ ] **Phase 5 — Synchronization:** one-way/two-way folder sync, conflicts, version history
- [ ] **Phase 6 — Sharing and security:** invited users, per-root permissions, share links, revocation
- [ ] **Phase 7 — Release:** Windows installer with service registration, auto-update, signed Android release

## License

FSL-1.1-ALv2: the Functional Source License, version 1.1, with Apache 2.0 as the future license. See [LICENSE.md](LICENSE.md).
