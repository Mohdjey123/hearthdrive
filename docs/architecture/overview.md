# Architecture overview

## Processes

```
┌──────────────────────┐   invoke('service_connection')   ┌─────────────────────────────┐
│ NEXUS Desktop (Tauri)│ ───────────────────────────────▶ │ reads runtime.json, starts  │
│ React UI in WebView2 │                                  │ nexus-service if not running│
└─────────┬────────────┘                                  └─────────────────────────────┘
          │ HTTP + WebSocket, Bearer <local token>
          ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│ nexus-service (HTTP on 127.0.0.1:47100; peer-to-peer endpoint on UDP)          │
│  nexus-api ── nexus-security (auth, tickets, roles)                           │
│     │                                                                         │
│     ├── nexus-storage  (roots, path validation, file ops, trash, uploads)     │
│     ├── nexus-indexer  (scanner, watcher, hasher, availability monitor)       │
│     ├── nexus-devices  (Ed25519 identity, signed requests, pairing links)     │
│     └── nexus-network  (iroh endpoint: QUIC dialed by device key, relays)     │
│  nexus-database (SQLite, WAL) ◀── every crate reads and writes through it     │
└──────────────────────────────────────────────────────────────────────────────┘
```

* The service runs independently of the window. Closing the UI never stops storage.
* A browser on the same computer can use the same UI: the service serves the built bundle and users sign in with their password.
* `runtime.json` in the data directory holds the port and a per-run **local token**. The data directory is inside the user's profile, so only that Windows user can read it.

## Data model

Each node owns its own SQLite database (`migrations/`). Databases are never shared or replicated. Nodes will exchange metadata events instead (Phase 4).

Files are stored relative to their storage root (`parent_path`, `relative_path`, `/`-separated, case-insensitive). An upsert keyed on `(root_id, relative_path)` keeps file ids stable across rescans, renames and moves. Sync and version history will depend on that later.

## Indexing

1. **Full scan:** walkdir on a blocking thread streams 2,000-entry batches to the async side, which upserts them with `indexed_at = scan start`. Rows not seen are then removed. Metadata only.
2. **Watcher:** `notify` events are debounced for 750 ms, collapsed to top-most paths, and re-indexed per subtree. Bursts of more than 2,000 paths fall back to a full scan.
3. **Hasher:** computes BLAKE3 for unhashed files slowly in the background and skips roots that are scanning. A hash is reset whenever size or mtime changes.
4. **Availability:** every 15 s it checks whether each root is reachable. Offline roots keep their index and are reported as `offline`. When a root reconnects it is watched and rescanned.

A scan whose root disappears mid-walk never prunes the index.

## Path safety

Every file operation resolves through `nexus_storage::paths::resolve`:

* rejects `..`, `.`, drive and UNC prefixes, `:` (alternate data streams), reserved device names (`CON`, `NUL`, `COM1`…), control characters, and trailing dots or spaces;
* reserves `.nexus/` (trash, staging) and system folders;
* canonicalizes the nearest existing ancestor and requires it to stay inside the canonical root, which defeats symlink and junction escapes;
* refuses a symlink or junction at the target itself.

Uploads stream to `.nexus/staging/*.part` on the same volume. They are hashed while they stream and placed with a hard link (an atomic no-clobber on NTFS), falling back to check-then-rename on FAT and exFAT. A file is never overwritten.

## API

`/api/v1`, documented by `crates/nexus-api/src/lib.rs` (routes) and the integration tests in `crates/nexus-api/tests/api.rs`. Media URLs use signed, expiring, file-scoped tickets, so `<img>` and `<video>` can load them without exposing the session token.

## Networking and devices (Phase 2)

```
 Travel Laptop (node B)                                    Studio PC (node A)
 ┌──────────────────────┐   QUIC + TLS 1.3, keyed by     ┌──────────────────────┐
 │ UI ─▶ 127.0.0.1      │   each node's device key       │ 127.0.0.1 (UI, API)  │
 │   /peers/A/proxy/…   │ ── HTTP/1.1 per stream ──────▶ │ iroh endpoint (UDP)  │
 │ iroh endpoint (UDP)  │    signed x-nexus-* headers    │  → restricted router │
 └──────────────────────┘ ◀─ hello every 20 s ───────────└──────────────────────┘
        direct path when possible (LAN, hole punching, UPnP); else via a relay
```

**Endpoint.** Each node runs an [iroh](https://iroh.computer) endpoint whose secret key is the node's device key, so a device's public key is also its network address. The TLS handshake proves both keys, and NEXUS never handles another key for networking. The endpoint binds UDP (port 47100 by default, or random if taken). It finds its public addresses, asks the router for a port mapping, and keeps a connection to a home relay. With the default relays it also publishes its relay address under its key through the iroh DNS service, so peers can find it again after a change. `NEXUS_RELAY` selects your own relays or none. The HTTP API never listens beyond `127.0.0.1`.

**Transport.** A request to a peer opens one bidirectional QUIC stream and speaks plain HTTP/1.1 on it (hyper on both sides). Connections are cached per peer and reopened once if a cached one has died. On the receiving side, `p2p::spawn_server` accepts connections only while remote access is on (the `network.remote_enabled` setting, on by default), and closes them when it is turned off. Each request then passes an admission check before the router sees it:
* the path must be in `files`, `search`, `recent`, `storage`, `trash`, `system`, `peer` or `pairing`;
* it must not carry an `Authorization` header;
* it must carry device signature headers, unless it is `POST /pairing/request`.

The request is tagged with the connection's authenticated key (`P2pPeer`) and a synthetic `fd00::/8` client address (for throttling). Device authentication requires that tag and that the signing device's stored key equals it. Device-signed requests on the loopback HTTP port are refused.

**Status.** `spawn_network_monitor` republishes the endpoint's state every 10 seconds and whenever its addresses or relay connection change:
* `online`: a relay is connected;
* `starting`: no relay yet, during the first 20 seconds;
* `local_only`: no relay, so only direct connections are possible;
* `off`: remote access is turned off.

**Identity.** Each node has an Ed25519 key in `identity.key`, encrypted with DPAPI for the Windows user. Its fingerprint is the first 128 bits of BLAKE3(public key), shown as eight groups of four hex digits.

**Pairing.**
1. Host: a person creates an invitation. The link `nexus-pair:<base64url JSON>` carries the host's device id, name, public key and address hints (`relay:<url>` and `ip:port`), plus a single-use 128-bit code that expires after 10 minutes. Only the code's hash is stored, and creating a new invitation closes older ones.
2. Joiner: it dials the host's key. The QUIC handshake already proves the host holds that key. It then sends `POST /pairing/request` with its own id, name, public key, address hints and a signature proving it holds that key. The host also requires that this key equals the one that authenticated the connection. Wrong codes count toward a per-key throttle.
3. Host: it records a pending request and signs an acknowledgement. The joiner checks that acknowledgement against the public key from the link.
4. Host: the owner sees the joiner's fingerprint and approves or declines.
5. Joiner: it polls `GET /pairing/requests/{id}/status`, signed with its key, over a connection made with that key. The host signs the reply. On approval each side stores the other's public key and address hints; a device gets the **editor** role by default, which can be changed per device.

**Signed requests.** Peer requests carry `x-nexus-device`, `-timestamp`, `-nonce`, `-signature`, `-acting-user` and `-acting-role`. The signature covers method, path and query, timestamp, nonce, the target device id, the acting role and the acting user. The receiver requires a known, paired key equal to the connection's key, a timestamp within ±120 s and a nonce it has not seen. The effective role is the lower of the device's role and the person's role on their own node. Bodies are streamed, not signed; QUIC's TLS 1.3 provides confidentiality and integrity on the wire.

**Browsing.** The UI calls its own node at `/api/v1/peers/{device}/proxy/{rest}`. The node accepts only people signed in locally (never another device, so requests cannot hop) and only the `files`, `search`, `recent`, `storage`, `trash` and `system` areas. It forwards the request signed over the peer connection and streams the response back. Media URLs carry the remote node's own content ticket, so `<img>` and `<video>` work without exposing a session.

**Presence and removal.** Every 20 seconds each node calls `GET /peer/hello` on its peers, and sooner after a failed proxy call. The reply signs the caller's nonce and carries the peer's current address hints. The node records how the connection flows (`direct ip:port` or `relay url`) and its round-trip time. Removing a device deletes its key at once, so its next request is refused, sends a signed `POST /peer/unpair` so the other side forgets this node too, and closes the cached connection.

**Relays.** A relay only forwards encrypted QUIC packets between endpoints it cannot decrypt. The public relays run by the iroh project are rate-limited, which slows large relayed transfers; direct paths are not affected. A self-hosted `iroh-relay` removes that limit.

## Across devices, access and accounts (Phases 4 and 6)

**Search across devices.** There is no shared index. The UI asks its own node and, through the proxy, each online paired device, and merges the answers. Each result carries the device it came from, so opening, previewing and downloading use that device's API.

**Copy between devices.** `POST /transfers/copy` takes a source (`from_device`, `file_id`) and a destination (`to_device`, `root_id`, `path`); either side can be this node. The node opens the source as a stream (a local file, or the peer's ticketed content URL) and feeds it to the destination (the local upload path, or the peer's `/files/upload`). Only people signed in here may call it. Uploads to a peer are answered only when the whole body has arrived, so requests with a body use a 12-hour limit instead of the 20-second one.

**Per-device storage access.** `devices.allowed_roots` is `NULL` (everything) or a JSON list of storage root ids. It is loaded into the caller's identity when a device request is authenticated, and every file route checks it: listing roots and trash filters, search and recent filter, and anything addressed to a hidden root or a file in one answers "not found".

**Per-person storage access.** `users.allowed_roots` works the same way for people, except that its list may name storage locations on paired devices too. Owners and administrators are never limited (they could lift their own limit). The list is loaded with the session on every request, so a change applies at once. When a limited person's request goes to a paired device (the proxy, copies, sync), the node adds `x-nexus-acting-roots` with the list, and the signature covers it (an extra `roots:` line in the signed text, present only for limited people, so other requests sign exactly as before). The device then applies the intersection of its own limit for the calling node and the person's list. Limited people also only receive live events for their locations, only see their own entries in the activity log and their own transfers, and only see sync pairs whose folder here they can use; a sync pair stops running if its creator loses that folder.

**Accounts.** `/users` lets owners and administrators add, change and remove accounts. Rules enforced on the server: only an owner can create, change or remove an owner; the last owner cannot be removed or demoted; nobody can remove their own account; removing an account or changing its password deletes its sessions. Devices can never call these routes.

**Share links.** `POST /shares` (`file_id`, `provider`, `expires_secs`, `downloads`, `password`) records a link in `shares` and starts a job of kind `share`; `GET /shares`, `POST /shares/refresh` (asks the service for downloads used and time left) and `DELETE /shares/{id}` (deletes the file from the service) complete the set, and `/shares/services` reads or sets the Send instance (`share.send_url`). Editors and above may share; owners and administrators see every link, others only their own (by the same owner key as resumable uploads). `shares` is reachable over the peer-to-peer network and the proxy, so a phone shares a computer's files and the computer does the work.

The Send provider (`nexus-api/src/share.rs`) follows the protocol of [timvisee/send](https://github.com/timvisee/send) exactly, checked against values computed with the browser's Web Crypto API: a random 16-byte secret per link; HKDF-SHA256 derives the metadata key (AES-128-GCM over the name, size and type) and the 64-byte authentication key the service verifies before a download; the content is encrypted as RFC 8188 `aes128gcm` records of 64 KiB with a random salt, streamed over a WebSocket to `/api/ws`. The link is `{url}#{secret}`; the part after `#` never leaves the recipient's browser, so the service holds only ciphertext. A password replaces the authentication key with PBKDF2-SHA256(password, full link) and is not stored. The owner token that lets this node delete the file stays in the local database and is dropped when the link expires. Folders are packed into a zip under `<data dir>/tmp` first and removed afterwards. temp.sh is a plain multipart upload with no encryption, no early deletion and a fixed three days. TLS uses rustls with built-in root certificates, the same on every platform.

**Updates.** A release (`nexus-devices/src/release.rs`) is the JSON text `{platform, version, file, size, blake3, notes, published_at}` plus an Ed25519 signature over `nexus-release-v1
` and that exact text; nodes accept signatures from `TRUSTED_KEYS` only. Each node keeps at most one package per platform in `<data dir>/updates/<platform>/` (`release.json` and `package.exe` or `package.apk`); the `nexus-release` tool signs a package and adds it there. `GET /peer/updates` lists a node's signed releases and `GET /peer/updates/{platform}` streams a package, to paired devices only. A background task (`routes/updates.rs`) asks online peers every 30 minutes, picks the newest trusted release per platform that is newer than both its stored package and, for its own platform, the running version, streams it into `download.part` while enforcing the signed size, and keeps it only if the BLAKE3 hash matches. `GET /updates`, `POST /updates/check`, `PUT /updates/settings` and `POST /updates/install` serve the app. On Windows the service checks the hash again and starts the NSIS installer with `/P /UPDATE` (plus `/R` when you asked from the window, to reopen it); the installer hooks in `apps/desktop/src-tauri/windows/installer-hooks.nsh` stop `nexus-service.exe` before copying and start it again afterwards. Automatic installs wait until no app window holds the event stream open and no job is running, and the same version is not retried for six hours. On Android the app passes the package path to `MainActivity.installUpdate`, which accepts only the node's own update file, copies it to the cache folder and opens Android's installer through the file provider.

**Start with Windows.** `PUT /system/autostart` writes or removes `NEXUS Cloud` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, pointing at the running service executable. It is accepted from loopback only.

## Resumable transfers, versions and sync (Phases 4 and 5)

**Resumable uploads.** `POST /files/uploads` records the destination and size (`uploads` table) and validates the name, folder and free space before any byte is sent. Bytes go to `.nexus/staging/<id>.part` with `PUT /files/uploads/{id}?offset=N`; the offset must equal what has arrived, so a piece can never be written twice or out of order. Whatever arrives before a connection drops is kept. `GET` answers the current offset; `POST …/complete` hashes the file (BLAKE3), applies the original modification time and places it atomically. Only the person who started an upload can continue it. Uploads untouched for a day are deleted.

**Copy jobs.** `POST /transfers/copy` answers at once with a job; `GET /transfers` lists jobs with progress. Each file goes through a resumable upload at the destination, so when either connection drops the job asks how much arrived, reopens the source at that offset (a local seek, or an HTTP range request to the peer) and continues, up to eight times with back-off. Folders are listed with `GET /files/tree` and recreated under a free name. Two jobs run at a time.

**Versions.** Uploads with `on_conflict=replace` move the existing file to `.nexus/versions/<version id>` and record it in `file_versions` before putting the new one in place; the file keeps its id. Ten versions are kept per file. Restoring swaps the version back and keeps the replaced content as a version.

**Sync.** A pair (`sync_pairs`) lives only on the node that created it; the other device just answers ordinary signed file requests, acting for the person who set it up (with that person's current role, capped by the device's role there). A run lists both folders (`/files/tree`), compares every path, case-insensitively, with the fingerprints (size and modification time) both sides had after the last run (`sync_state`), and plans:

| Here | There | Two-way | Upload | Download |
| --- | --- | --- | --- | --- |
| changed | unchanged | copy there | copy there | copy here |
| unchanged | changed | copy here | copy there | copy here |
| deleted | unchanged | trash there | trash there | copy here |
| changed | changed | newer wins, other kept as a conflict copy | copy there | copy here |

The planner is a pure function with unit tests. Replacements keep versions; removals go to the trash. A run that would remove more than 20 files and more than half of the folder skips the removals until someone presses Sync now. A folder still being indexed, or offline, is not synced, so an incomplete listing never looks like deleted files. Temporary files (`~$*`, `*.tmp`, `*.part`, `desktop.ini`, `Thumbs.db`) are ignored.

Runs happen about 20 seconds after a change in the local folder, every five minutes otherwise (changes on the device are only noticed by looking), and on demand.

## Android app (Phase 3)

The Tauri shell (`apps/desktop/src-tauri`) builds for Windows and Android from one crate:

* **Desktop.** `service_connection` finds or starts the separate `nexus-service` process and returns its address and local token.
* **Android.** There is no separate process. `service_connection` starts the node in the app's own process on first use (`nexus_core::run_with` on a dedicated thread and runtime), with its data in the app's private directory and the API on a random loopback port. The web view talks to it over loopback HTTP; the network security config allows plain HTTP to `127.0.0.1` only.

The UI is the same React app. It asks the shell which host it runs in (`shell: desktop | mobile`) and adapts:
* a bottom tab bar replaces the sidebar on narrow screens, and tables drop their secondary columns;
* tap opens, press-and-hold selects;
* setup asks only for a phone name. The account gets a random password, because the app always signs in to its own node with the per-run local token;
* pairing can scan the QR code (`tauri-plugin-barcode-scanner`);
* downloads go through the `save_download` command, which streams the file from the app's own node into `Download/NEXUS` (a web view cannot save files on Android). It refuses any URL that is not on the app's own node.

On Android the device key is stored in the app's private storage without DPAPI; the app sandbox protects it, and the app opts out of Android backups and device transfers (`allowBackup="false"`, `data_extraction_rules.xml`), so the key and sessions never leave the phone. Every app on a phone can reach the loopback port, so the embedded node only accepts first-run setup with its local token (`Config::embedded` sets `setup_needs_token`). The node stops when Android stops the app, so a phone is reachable by other devices only while the app is open.

**Phone backup.** `backups` (one row) names a paired computer, storage location and folder, and which phone folders to read (`DCIM`, `Pictures`, `Movies` under `/storage/emulated/0`); `backup_files` remembers each file sent with its size and modification time. A run (`routes/backup.rs`) lists the phone folders (skipping hidden files, which is how Android marks files being written or in its trash, and files changed in the last 30 seconds), drops what `backup_files` already has, skips files the computer already has with the same name and size, and sends the rest as one job through the same resumable upload as copies (`Mover::send_disk_file`), with `make_parents` and the photo's date. A changed photo is sent with `replace` (the computer keeps a version); a new one with `rename`, so a different file of the same name is never overwritten. It runs 15 seconds after the app starts, every 15 minutes, two minutes after a failure, and on request. The app asks for `READ_MEDIA_IMAGES`/`READ_MEDIA_VIDEO` through a small `window.NexusAndroid` bridge in `MainActivity`.

**Share menu.** `MainActivity` accepts `SEND` and `SEND_MULTIPLE` for any type. It copies each shared `content://` item (never `file://`, never the app's own provider) into `<cache>/shared` as `name.part`, renames it when complete, and tells the page with a `nexus-shared` event. `GET /shared` lists the inbox; `POST /shared/send` sends chosen files (plain names only) to a folder on a paired device as a job and deletes each from the inbox once it arrived; `POST /shared/discard` drops them.

Still to come: shared users across nodes, a unified index (Phase 4), a foreground service for long transfers on Android, and an installer firewall rule for the UDP port (Phase 7).
