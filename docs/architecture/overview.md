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

Still to come: shared users across nodes, a unified index (Phase 4), Android access (Phase 3), and an installer firewall rule for the UDP port (Phase 7).
