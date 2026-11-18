// Typed client for the nexus-service `/api/v1` API.

export type Availability =
  | 'online' | 'offline' | 'synchronizing' | 'downloading' | 'uploading' | 'missing' | 'error'

export type Role = 'owner' | 'administrator' | 'editor' | 'contributor' | 'viewer' | 'restricted'

export interface FileEntry {
  id: string
  root_id: string
  relative_path: string
  parent_path: string
  filename: string
  is_dir: boolean
  extension: string | null
  mime_type: string | null
  size: number
  created_at: number | null
  modified_at: number | null
  content_hash: string | null
  indexed_at: number
  availability: Availability
}

export interface RootSummary { id: string; name: string; online: boolean; read_only: boolean }
export interface FileDetails extends FileEntry { root: RootSummary }
export interface FoundItem extends FileEntry { root_name: string }

export interface ScanState {
  root_id: string
  status: 'idle' | 'scanning' | 'error' | 'offline'
  files_seen: number
  bytes_seen: number
  started_at: number | null
  completed_at: number | null
  error: string | null
}

export interface StorageRoot {
  id: string
  device_id: string
  name: string
  absolute_path: string
  root_type: 'managed' | 'existing' | 'external'
  read_only: boolean
  enabled: boolean
  created_at: number
  online: boolean
  scanning: boolean
  stats: { files: number; dirs: number; bytes: number }
  scan: ScanState | null
  disk: { total_bytes: number; available_bytes: number } | null
}

export interface Drive {
  mount_point: string
  label: string
  file_system: string
  total_bytes: number
  available_bytes: number
  removable: boolean
  kind: string
}

export interface Device {
  id: string
  name: string
  device_type: string
  public_key: string | null
  network_address: string | null
  is_local: boolean
  last_seen: number | null
  status: string
  created_at: number
  role: Role
  latency_ms: number | null
  paired_at: number | null
  version: string | null
  online: boolean
  fingerprint: string | null
  addresses: string[]
  /** How traffic to the device flows right now: `direct <ip:port>` or `relay <url>`. */
  route: string | null
  /** Storage locations on this node the device may see; `null` means all. */
  allowed_roots: string[] | null
  roots: number
  indexed_bytes: number
  /** The paired device that introduced this one; `null` if paired directly. */
  introduced_by: string | null
  /** When it was blocked: its requests are refused and nothing is sent to it. */
  blocked_at: number | null
}

export type NetworkState = 'off' | 'starting' | 'online' | 'local_only'

export interface NetworkStatus {
  provider: 'p2p'
  display_name: string
  state: NetworkState
  connected: boolean
  detail: string
  relay_mode: 'default' | 'custom' | 'disabled'
  relay_url: string | null
  relay_error: string | null
  direct_addresses: string[]
  address_lookup: boolean
}

export interface NetworkInfo {
  status: NetworkStatus
  remote_enabled: boolean
  /** Ways other devices can reach this one: `relay:<url>` or `ip:port`. */
  advertised: string[]
  updated_at: number
  generation: number
  device_id: string
  fingerprint: string
}

export interface Invite { session_id: string; link: string; expires_at: number; fingerprint: string; addresses: string[] }

export interface PairingRequest {
  id: string
  session_id: string
  device_id: string
  name: string
  device_type: string
  public_key: string
  version: string | null
  remote_ip: string | null
  status: string
  created_at: number
  fingerprint: string | null
  addresses: string[]
}

export interface JoinState {
  id: string
  host_device_id: string
  host_name: string
  host_fingerprint: string
  /** How the host was reached: `direct <ip:port>` or `relay <url>`. */
  route: string | null
  status: 'waiting' | 'approved' | 'rejected' | 'expired' | 'failed'
  error: string | null
  created_at: number
}

export interface User {
  id: string
  username: string
  role: Role
  created_at: number
  /** Storage locations (here or on paired devices) the person is limited to; `null` = all. */
  allowed_roots: string[] | null
}
/** An account as People lists it, with how it signs in. */
export interface Person extends User {
  two_factor: boolean
  passkeys: number
  locked_at: number | null
}
export interface Me { user_id: string | null; username: string; role: Role; local: boolean; device_id: string | null }
export interface Session { token: string; expires_at: number; user: User }
/** The password was right; a code from the authenticator app is needed next. */
export interface CodeNeeded { two_factor: true; ticket: string }

export interface Passkey { id: string; name: string; created_at: number; last_used_at: number | null }
export interface SignInSession {
  id: string
  /** The browser's user agent. */
  client: string | null
  /** password, password+code, password+recovery, passkey or setup. */
  method: string | null
  created_at: number
  last_used_at: number | null
  expires_at: number
  current: boolean
}
export interface SecurityStatus {
  two_factor: boolean
  two_factor_since: number | null
  recovery_codes_left: number
  passkeys: Passkey[]
  sessions: SignInSession[]
  /** Whether passkeys can be used from this page. */
  passkeys_here: boolean
}
/** What the browser needs to make a passkey (base64url fields). */
export interface PasskeyOptions {
  challenge: string
  rp: { id: string; name: string }
  user: { id: string; name: string; display_name: string }
  algorithms: number[]
  exclude: string[]
}
export interface PasskeyAnswer {
  credential_id: string
  client_data: string
  authenticator_data: string
  signature: string
  user_handle: string | null
}

export interface SystemInfo {
  node: { device_id: string; name: string; hostname: string; version: string; platform: string }
  started_at: number
  uptime_ms: number
  data_dir: string
  network: Omit<NetworkInfo, 'device_id' | 'fingerprint'>
  fingerprint: string
}

export interface ListResponse {
  root: RootSummary
  path: string
  items: FileEntry[]
  total: number
  offset: number
  limit: number
}

export interface TrashItem {
  id: string
  root_id: string
  root_name: string
  original_path: string
  filename: string
  is_dir: boolean
  size: number
  deleted_at: number
}

/** An earlier content of a file, kept when it was replaced. */
export interface FileVersion {
  id: string
  file_id: string
  size: number
  modified_at: number | null
  content_hash: string | null
  created_at: number
  created_by: string | null
}

/** A link anyone can download a file or folder with, until it expires. */
export interface ShareLink {
  id: string
  file_id: string
  root_id: string
  /** What recipients download (a folder becomes "Name.zip"). */
  name: string
  is_dir: boolean
  size: number
  /** `send` is end-to-end encrypted; `tempsh` is not. */
  provider: 'send' | 'tempsh'
  service: string
  /** The full link, key included; set once the upload finished. */
  url: string | null
  expires_at: number
  downloads: number | null
  downloaded: number
  has_password: boolean
  status: 'uploading' | 'active' | 'expired' | 'failed'
  error: string | null
  job_id: string | null
  created_by: string
  created_at: number
  bytes_done: number | null
  bytes_total: number | null
}

export interface ShareServices {
  send_url: string
  send: { MAX_FILE_SIZE: number; MAX_DOWNLOADS: number; MAX_EXPIRE_SECONDS: number } | null
  send_error: string | null
  tempsh_max_size: number
  tempsh_expire_secs: number
  can_configure: boolean
}

/** A copy or sync running on this node. */
export interface Job {
  id: string
  kind: 'copy' | 'sync' | 'backup' | 'share' | 'check'
  title: string
  /** "This computer → Laptop" */
  route: string
  by: string
  created_at: number
  status: 'queued' | 'running' | 'paused' | 'done' | 'failed' | 'cancelled'
  /** Why it waits (transfer hours). */
  note: string | null
  can_retry: boolean
  verified: number
  files_total: number
  files_done: number
  bytes_total: number
  bytes_done: number
  current: string | null
  errors: string[]
  retries: number
  started_at: number | null
  finished_at: number | null
}

export interface BackupSchedule {
  every_hours: number
  keep_last: number
  keep_daily: number
  keep_weekly: number
  keep_monthly: number
}

export interface Snapshot {
  id: string
  set_id: string
  taken_at: number
  files: number
  bytes: number
  new_files: number
  new_bytes: number
}

/** A folder backed up to a paired device as snapshots. */
export interface SnapshotSet extends BackupSchedule {
  id: string
  name: string
  local_root_id: string
  local_path: string
  device_id: string
  remote_root_id: string
  remote_path: string
  remote_label: string
  enabled: boolean
  created_by: string
  created_at: number
  last_run_at: number | null
  last_ok_at: number | null
  last_result: string | null
  last_error: string | null
  last_check_at: number | null
  last_check_result: string | null
  /** Contents and snapshot lists are encrypted on the device. */
  encrypted: boolean
  device_name: string
  local_label: string
  running: boolean
  snapshots: number
  latest: Snapshot | null
}

export interface SnapshotEntry {
  name: string
  path: string
  is_dir: boolean
  size: number
  modified_at: number | null
  files: number
}

export interface TransferSettings {
  /** Transfers running at the same time (1–4). */
  parallel: number
  /** KB/s for all copies, syncs and backups together; 0 for no limit. */
  limit_kbps: number
  /** "HH:MM", local time; both null for any time. */
  hours_start: string | null
  hours_end: string | null
}

export interface Backup {
  id: string
  device_id: string
  device_name: string
  root_id: string
  path: string
  /** "Storage / folder" on the computer. */
  label: string
  sources: string[]
  enabled: boolean
  created_by: string
  last_run_at: number | null
  last_ok_at: number | null
  last_result: string | null
  last_error: string | null
  created_at: number
}

export interface BackupStatus {
  /** Whether this device has photos to back up (phones). */
  available: boolean
  choices: { key: string; label: string }[]
  backup: Backup | null
  running: boolean
}

export interface SharedFile { name: string; size: number }

export interface UpdateRelease {
  platform: string
  version: string
  file: string
  size: number
  notes: string
  published_at: number
}

export interface UpdateStatus {
  version: string
  platform: string | null
  /** A newer version for this device, ready to install. */
  available: UpdateRelease | null
  /** `service`: this computer installs it; `app`: Android's installer; `none`. */
  install: 'service' | 'app' | 'none'
  install_note: string | null
  package_path: string | null
  auto_install: boolean
  can_configure: boolean
  checking: boolean
  downloading: string | null
  last_checked: number | null
  error: string | null
  /** Packages this device keeps and passes on to your other devices. */
  carried: UpdateRelease[]
}

/** The Android app's bridge (phones only). */
interface AndroidBridge {
  hasMediaAccess(): boolean
  requestMediaAccess(): void
  openAppSettings(): void
  /** Opens Android's installer for the node's verified update package. */
  installUpdate?(path: string): 'started' | 'permission' | 'missing'
}

export function androidBridge(): AndroidBridge | null {
  return (window as unknown as { NexusAndroid?: AndroidBridge }).NexusAndroid ?? null
}

export type SyncMode = 'two_way' | 'upload' | 'download' | 'copy'

export interface SyncPair {
  id: string
  local_root_id: string
  local_path: string
  local_label: string
  device_id: string
  device_name: string
  remote_root_id: string
  remote_path: string
  remote_label: string
  mode: SyncMode
  enabled: boolean
  created_by: string
  last_run_at: number | null
  last_ok_at: number | null
  last_result: string | null
  last_error: string | null
  created_at: number
  running: boolean
  /** Copies: days a file removed here stays on the copy. */
  keep_days: number
  /** Copies: the last read-back of the copy. */
  last_check_at: number | null
  last_check_result: string | null
  checking: boolean
  /** Copies: files removed here that the copy still keeps. */
  held_files: number
  held_bytes: number
}

/** A file removed here that a copy still keeps. */
export interface HeldFile {
  path: string
  is_dir: boolean
  size: number
  gone_at: number
}

/** What Windows reports about a physical disk. */
export interface PhysicalDisk {
  number: string
  model: string
  media: 'hdd' | 'ssd' | 'unknown'
  bus: string
  health: 'healthy' | 'warning' | 'unhealthy' | 'unknown'
  status: string
  size: number
  letters: string[]
  temperature_c: number | null
  temperature_max_c: number | null
  wear_percent: number | null
  power_on_hours: number | null
  read_errors: number | null
  write_errors: number | null
  /** Whether temperature, wear and errors could be read (needs administrator rights). */
  details: boolean
  /** NEXUS storage locations on this disk. */
  storage: string[]
}

export interface DamagedFile {
  file_id: string
  path: string
  size: number
  found_at: number
  unreadable: boolean
  /** Where a repair would take the right content from. */
  repair_from: string[]
}

export interface RootHealth {
  id: string
  name: string
  path: string
  online: boolean
  since: number | null
  removable: boolean
  total_bytes: number | null
  available_bytes: number | null
  disk: string | null
  last_check: { checked_at: number; files: number; bytes: number; damaged: number; result: string } | null
  verifying: boolean
  damaged: DamagedFile[]
}

export interface StorageHealth {
  checked_at: number | null
  disks: PhysicalDisk[]
  disks_error: string | null
  roots: RootHealth[]
  problems: { level: 'error' | 'warning'; text: string; root_id: string | null }[]
}

interface UploadProgress { id: string; offset: number; size: number }

export interface ActivityEntry {
  id: string
  action: string
  resource: string | null
  username: string | null
  timestamp: number
}

export class ApiError extends Error {
  readonly status: number
  readonly code: string
  constructor(status: number, code: string, message: string) {
    super(message)
    this.status = status
    this.code = code
  }
}

interface Connection { baseUrl: string; token: string | null; local: boolean; dataDir?: string; shell?: 'desktop' | 'mobile' }

const TOKEN_KEY = 'nexus.session'
let conn: Connection = { baseUrl: '', token: null, local: false }
let unauthorizedHandler: (() => void) | null = null

export const isTauri = () => typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

/** Resolves the service address: from the Tauri shell, or the page origin. */
export async function initConnection(): Promise<Connection> {
  if (isTauri()) {
    const { invoke } = await import('@tauri-apps/api/core')
    const c = await invoke<{ base_url: string; token: string; data_dir: string; shell?: 'desktop' | 'mobile' }>('service_connection')
    conn = { baseUrl: c.base_url, token: c.token, local: true, dataDir: c.data_dir, shell: c.shell ?? 'desktop' }
  } else {
    let stored: string | null = null
    try { stored = localStorage.getItem(TOKEN_KEY) } catch { /* storage unavailable */ }
    const base = (import.meta.env.VITE_NEXUS_API as string | undefined) ?? window.location.origin
    conn = { baseUrl: base.replace(/\/$/, ''), token: stored, local: false }
  }
  return conn
}

export const connection = () => conn

/** The UI runs inside the phone app, which hosts its own node. */
export const isMobile = () => conn.shell === 'mobile'

/** Touch-first input: tap opens, press-and-hold selects. */
export const isTouch = () =>
  isMobile() || (typeof window !== 'undefined' && window.matchMedia?.('(pointer: coarse)').matches === true)

/** "phone" or "computer", for wording that names this device. */
export const thisDevice = () => (isMobile() ? 'phone' : 'computer')

export function setSessionToken(token: string | null) {
  if (conn.local) return
  conn.token = token
  try {
    if (token) localStorage.setItem(TOKEN_KEY, token)
    else localStorage.removeItem(TOKEN_KEY)
  } catch { /* storage unavailable */ }
}

export function onUnauthorized(handler: () => void) {
  unauthorizedHandler = handler
}

export const absoluteUrl = (path: string) => conn.baseUrl + path

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {}
  if (conn.token) headers.Authorization = `Bearer ${conn.token}`
  if (body !== undefined) headers['Content-Type'] = 'application/json'
  let res: Response
  try {
    res = await fetch(`${conn.baseUrl}/api/v1${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    })
  } catch {
    throw new ApiError(0, 'unreachable', 'The NEXUS service is not reachable')
  }
  if (!res.ok) {
    let code = 'error'
    let message = res.statusText || `Request failed (${res.status})`
    try {
      const j = await res.json()
      code = j.error?.code ?? code
      message = j.error?.message ?? message
    } catch { /* not JSON */ }
    if (res.status === 401 && !path.startsWith('/auth/login')) unauthorizedHandler?.()
    throw new ApiError(res.status, code, message)
  }
  if (res.status === 204) return undefined as T
  // 202 Accepted may or may not carry a body (a started job does).
  const text = await res.text()
  return (text ? JSON.parse(text) : undefined) as T
}

/** Like `request`, for calls that answer 202 Accepted with a body. */
async function requestJson<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  if (conn.token) headers.Authorization = `Bearer ${conn.token}`
  let res: Response
  try {
    res = await fetch(`${conn.baseUrl}/api/v1${path}`, { method, headers, body: JSON.stringify(body) })
  } catch {
    throw new ApiError(0, 'unreachable', 'The NEXUS service is not reachable')
  }
  const j = await res.json().catch(() => null)
  if (!res.ok) {
    if (res.status === 401) unauthorizedHandler?.()
    throw new ApiError(res.status, j?.error?.code ?? 'error', j?.error?.message ?? `Request failed (${res.status})`)
  }
  return j as T
}

const q = (params: Record<string, string | number | boolean | undefined | null>) => {
  const s = new URLSearchParams()
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== null && v !== '') s.set(k, String(v))
  const str = s.toString()
  return str ? `?${str}` : ''
}

/**
 * File browsing calls. With a prefix of `/peers/{id}/proxy` the same calls go
 * to a paired device, relayed and signed by this computer's node.
 */
function createFileApi(prefix: string) {
  const r = <T,>(method: string, path: string, body?: unknown) => request<T>(method, prefix + path, body)
  return {
    prefix,
    systemInfo: () => r<SystemInfo>('GET', '/system/info'),
    roots: () => r<StorageRoot[]>('GET', '/storage/roots'),
    list: (p: { root_id: string; path?: string; sort?: string; order?: string; offset?: number; limit?: number }) =>
      r<ListResponse>('GET', `/files${q(p)}`),
    file: (id: string) => r<FileDetails>('GET', `/files/${id}`),
    link: (id: string, download = false) =>
      r<{ url: string; expires_in: number }>('POST', `/files/${id}/link${download ? '?download=1' : ''}`),
    createFolder: (root_id: string, path: string, name: string) =>
      r<FileDetails>('POST', '/files/folder', { root_id, path, name }),
    updateFile: (id: string, b: { name?: string; parent_path?: string }) => r<FileDetails>('PATCH', `/files/${id}`, b),
    deleteFile: (id: string) => r<TrashItem>('DELETE', `/files/${id}`),
    restoreFile: (id: string) => r<FileDetails>('POST', `/files/${id}/restore`),
    trash: () => r<TrashItem[]>('GET', '/trash'),
    purge: (id: string) => r<void>('DELETE', `/trash/${id}`),
    search: (query: string, root_id?: string) =>
      r<{ items: FoundItem[]; has_more: boolean }>('GET', `/search${q({ q: query, root_id, limit: 200 })}`),
    recent: (limit = 10) => r<FoundItem[]>('GET', `/recent${q({ limit })}`),
    versions: (id: string) => r<FileVersion[]>('GET', `/files/${id}/versions`),
    restoreVersion: (id: string, version: string) => r<FileDetails>('POST', `/files/${id}/versions/${version}/restore`),
    deleteVersion: (id: string, version: string) => r<void>('DELETE', `/files/${id}/versions/${version}`),
    /** A download URL for an earlier version (absolute, ready to fetch). */
    versionUrl: async (id: string, version: string) => {
      const { url } = await r<{ url: string }>('POST', `/files/${id}/versions/${version}/link`)
      return absoluteUrl(prefix ? url.replace(/^\/api\/v1/, `/api/v1${prefix}`) : url)
    },
    shares: () => r<ShareLink[]>('GET', '/shares'),
    refreshShares: () => r<ShareLink[]>('POST', '/shares/refresh'),
    createShare: (b: { file_id: string; provider: 'send' | 'tempsh'; expires_secs?: number; downloads?: number; password?: string }) =>
      r<ShareLink>('POST', '/shares', b),
    deleteShare: (id: string) => r<void>('DELETE', `/shares/${id}`),
    shareServices: () => r<ShareServices>('GET', '/shares/services'),
    setShareServices: (send_url: string) => r<ShareServices>('PUT', '/shares/services', { send_url }),
  }
}

export type FileApi = ReturnType<typeof createFileApi>

export const peerPrefix = (deviceId: string) => `/peers/${encodeURIComponent(deviceId)}/proxy`

const fileApis = new Map<string, FileApi>()
/** File calls for this computer (`''`) or a paired device's proxy prefix. */
export function fileApi(prefix = ''): FileApi {
  let a = fileApis.get(prefix)
  if (!a) {
    a = createFileApi(prefix)
    fileApis.set(prefix, a)
  }
  return a
}

export const api = {
  ...fileApi(''),
  health: () => request<{ status: string; version: string }>('GET', '/health'),
  setupStatus: () => request<{ needs_setup: boolean; node_name: string }>('GET', '/setup/status'),
  setup: (b: { node_name: string; username: string; password: string }) => request<Session>('POST', '/setup', b),
  login: (username: string, password: string) => request<Session | CodeNeeded>('POST', '/auth/login', { username, password }),
  loginCode: (ticket: string, code: string) => request<Session>('POST', '/auth/login/code', { ticket, code }),
  passkeyStart: () => request<{ challenge: string; rp_id: string }>('POST', '/auth/passkey/start'),
  passkeySignIn: (b: PasskeyAnswer & { challenge: string }) => request<Session>('POST', '/auth/passkey', b),
  security: () => request<SecurityStatus>('GET', '/auth/security'),
  securityLog: (limit = 100) => request<ActivityEntry[]>('GET', `/auth/security/log${q({ limit })}`),
  setupTwoFactor: (password: string) => request<{ secret: string; uri: string }>('POST', '/auth/2fa/setup', { password }),
  enableTwoFactor: (code: string) => request<{ recovery_codes: string[] }>('POST', '/auth/2fa/enable', { code }),
  disableTwoFactor: (password: string, code: string) => request<void>('POST', '/auth/2fa/disable', { password, code }),
  newRecoveryCodes: (password: string, code: string) => request<{ recovery_codes: string[] }>('POST', '/auth/2fa/recovery-codes', { password, code }),
  addPasskeyStart: (password: string, code?: string) => request<PasskeyOptions>('POST', '/auth/passkeys/start', { password, code }),
  addPasskey: (b: { challenge: string; client_data: string; attestation_object: string; name: string }) => request<Passkey>('POST', '/auth/passkeys', b),
  removePasskey: (id: string) => request<void>('DELETE', `/auth/passkeys/${id}`),
  endSession: (id: string) => request<void>('DELETE', `/auth/sessions/${id}`),
  endOtherSessions: () => request<{ ended: number }>('POST', '/auth/sessions/end-others'),
  logout: () => request<void>('POST', '/auth/logout'),
  me: () => request<Me>('GET', '/auth/me'),
  activity: (limit = 20) => request<ActivityEntry[]>('GET', `/activity${q({ limit })}`),

  drives: () => request<Drive[]>('GET', '/storage/drives'),
  addRoot: (b: { path: string; name?: string; read_only?: boolean }) => request<StorageRoot>('POST', '/storage/roots', b),
  removeRoot: (id: string) => request<void>('DELETE', `/storage/roots/${id}`),
  rescanRoot: (id: string) => request<void>('POST', `/storage/roots/${id}/scan`),

  devices: () => request<Device[]>('GET', '/devices'),
  updateDevice: (id: string, b: { name?: string; role?: Role; allowed_roots?: string[]; all_roots?: boolean; blocked?: boolean }) =>
    request<void>('PATCH', `/devices/${id}`, b),

  users: () => request<Person[]>('GET', '/users'),
  createUser: (b: { username: string; password: string; role: Role }) => request<User>('POST', '/users', b),
  updateUser: (id: string, b: { role?: Role; password?: string; allowed_roots?: string[]; all_roots?: boolean; locked?: boolean; two_factor?: false }) =>
    request<User>('PATCH', `/users/${id}`, b),
  removeUser: (id: string) => request<void>('DELETE', `/users/${id}`),

  /** Starts copying files and folders between devices ('' or undefined = this
   *  device). Resolves at once; the copy runs in the background (see `jobs`). */
  copyItems: (b: { from_device?: string; file_ids: string[]; to_device?: string; root_id: string; path: string }) =>
    requestJson<Job>('POST', '/transfers/copy', b),
  jobs: () => request<Job[]>('GET', '/transfers'),
  cancelJob: (id: string) => request<void>('POST', `/transfers/${id}/cancel`),
  clearJobs: () => request<void>('POST', '/transfers/clear'),
  pauseJob: (id: string) => request<void>('POST', `/transfers/${id}/pause`),
  resumeJob: (id: string) => request<void>('POST', `/transfers/${id}/resume`),
  runJobNext: (id: string) => request<void>('POST', `/transfers/${id}/first`),
  retryJob: (id: string) => request<Job>('POST', `/transfers/${id}/retry`),
  transferSettings: () => request<TransferSettings & { can_configure: boolean }>('GET', '/transfers/settings'),
  setTransferSettings: (s: TransferSettings) => request<TransferSettings & { can_configure: boolean }>('PUT', '/transfers/settings', s),

  backup: () => request<BackupStatus>('GET', '/backup'),
  setBackup: (b: { device_id: string; root_id: string; path: string; sources: string[]; enabled?: boolean }) =>
    request<BackupStatus>('PUT', '/backup', b),
  updateBackup: (b: { enabled: boolean }) => request<BackupStatus>('PATCH', '/backup', b),
  removeBackup: () => request<void>('DELETE', '/backup'),
  runBackup: () => request<void>('POST', '/backup/run'),

  sharedFiles: () => request<SharedFile[]>('GET', '/shared'),
  sendShared: (b: { names: string[]; device_id: string; root_id: string; path: string }) =>
    requestJson<{ id: string }>('POST', '/shared/send', b),
  discardShared: (names: string[] = []) => request<void>('POST', '/shared/discard', { names }),

  snapshotSets: () => request<SnapshotSet[]>('GET', '/snapshots'),
  createSnapshotSet: (b: { local_root_id: string; local_path: string; device_id: string; remote_root_id: string; remote_path: string; schedule: BackupSchedule; encrypt: boolean }) =>
    request<SnapshotSet & { recovery_key: string | null }>('POST', '/snapshots', b),
  openSnapshotSet: (b: { device_id: string; remote_root_id: string; remote_path: string; recovery_key: string; local_root_id: string; local_path: string }) =>
    request<SnapshotSet>('POST', '/snapshots/open', b),
  snapshotRecoveryKey: (id: string, password: string, code?: string) =>
    request<{ recovery_key: string }>('POST', `/snapshots/${id}/recovery-key`, { password, code }),
  updateSnapshotSet: (id: string, b: { enabled?: boolean; schedule?: BackupSchedule }) => request<SnapshotSet>('PATCH', `/snapshots/${id}`, b),
  removeSnapshotSet: (id: string) => request<void>('DELETE', `/snapshots/${id}`),
  runSnapshotSet: (id: string) => request<void>('POST', `/snapshots/${id}/run`),
  checkSnapshotSet: (id: string) => request<void>('POST', `/snapshots/${id}/check`),
  snapshotHistory: (id: string) => request<Snapshot[]>('GET', `/snapshots/${id}/history`),
  snapshotFiles: (id: string, snap: string, path: string) =>
    request<SnapshotEntry[]>('GET', `/snapshots/${id}/${snap}/files?path=${encodeURIComponent(path)}`),
  restoreSnapshot: (id: string, snap: string, b: { paths: string[]; to: 'beside' | 'original' }) =>
    request<{ job_id: string; folder: string }>('POST', `/snapshots/${id}/${snap}/restore`, b),
  syncPairs: () => request<SyncPair[]>('GET', '/sync'),
  createSync: (b: {
    local_root_id: string
    local_path: string
    device_id: string
    remote_root_id: string
    remote_path: string
    mode: SyncMode
    keep_days?: number
  }) => request<SyncPair>('POST', '/sync', b),
  updateSync: (id: string, b: { enabled?: boolean; mode?: SyncMode; keep_days?: number }) => request<SyncPair>('PATCH', `/sync/${id}`, b),
  checkCopy: (id: string) => request<void>('POST', `/sync/${id}/check`),
  heldFiles: (id: string) => request<HeldFile[]>('GET', `/sync/${id}/held`),
  bringBack: (id: string, paths: string[]) => request<{ job_id: string }>('POST', `/sync/${id}/held/restore`, { paths }),
  /** Drive health here, or on a paired device with its proxy prefix. */
  storageHealth: (prefix = '') => request<StorageHealth>('GET', `${prefix}/storage/health`),
  refreshHealth: () => request<StorageHealth>('POST', '/storage/health/refresh'),
  verifyRoot: (id: string) => request<void>('POST', `/storage/roots/${id}/verify`),
  repairDamaged: (fileId: string) => request<{ job_id: string }>('POST', `/storage/health/damaged/${fileId}/repair`),
  acceptDamaged: (fileId: string) => request<void>('POST', `/storage/health/damaged/${fileId}/accept`),
  removeSync: (id: string) => request<void>('DELETE', `/sync/${id}`),
  runSync: (id: string) => request<void>('POST', `/sync/${id}/run`),

  updates: () => request<UpdateStatus>('GET', '/updates'),
  checkUpdates: () => request<UpdateStatus>('POST', '/updates/check'),
  setAutoUpdate: (auto_install: boolean) => request<UpdateStatus>('PUT', '/updates/settings', { auto_install }),
  installUpdate: () => request<UpdateRelease>('POST', '/updates/install'),
  introductions: () => request<{ enabled: boolean }>('GET', '/devices/introductions'),
  setIntroductions: (enabled: boolean) => request<{ enabled: boolean }>('PUT', '/devices/introductions', { enabled }),
  autostart: () => request<{ supported: boolean; enabled: boolean }>('GET', '/system/autostart'),
  setAutostart: (enabled: boolean) => request<{ supported: boolean; enabled: boolean }>('PUT', '/system/autostart', { enabled }),
  removeDevice: (id: string) => request<void>('DELETE', `/devices/${id}`),
  createInvite: () => request<Invite>('POST', '/devices/pair'),
  cancelInvite: (sessionId: string) => request<void>('DELETE', `/devices/pair/${sessionId}`),
  pairingRequests: () => request<PairingRequest[]>('GET', '/devices/pair/requests'),
  approvePairing: (id: string, role?: Role) =>
    request<{ device_id: string }>('POST', `/devices/pair/requests/${id}/approve`, { role }),
  rejectPairing: (id: string) => request<void>('POST', `/devices/pair/requests/${id}/reject`),
  join: (link: string) => request<JoinState>('POST', '/pairing/join', { link }),
  joinStatus: (id: string) => request<JoinState>('GET', `/pairing/join/${id}`),

  network: () => request<NetworkInfo>('GET', '/network'),
  refreshNetwork: () => request<NetworkInfo>('POST', '/network/refresh'),
  setRemoteAccess: (remote_enabled: boolean) => request<NetworkInfo>('PUT', '/network', { remote_enabled }),
}

/**
 * Signed URL for previews and streaming (no auth header needed). For a paired
 * device the ticket is issued by that device and fetched through the proxy.
 */
export async function contentUrl(id: string, download = false, prefix = '') {
  const { url } = await fileApi(prefix).link(id, download)
  return absoluteUrl(prefix ? url.replace(/^\/api\/v1/, `/api/v1${prefix}`) : url)
}

/** Files larger than this are sent in pieces that survive a dropped connection. */
const CHUNK = 8 * 1024 * 1024
const RESUME_KEY = 'nexus.uploads'

/** Unfinished uploads by file, so choosing the same file again continues it. */
function resumable(key: string, id?: string | null): string | undefined {
  try {
    const all = JSON.parse(localStorage.getItem(RESUME_KEY) ?? '{}') as Record<string, string>
    if (id === undefined) return all[key]
    if (id === null) delete all[key]
    else all[key] = id
    localStorage.setItem(RESUME_KEY, JSON.stringify(all))
  } catch { /* storage unavailable: uploads still work, they just restart */ }
  return undefined
}

/** Worth another try: the connection dropped or the service is busy. */
const retryable = (e: unknown) =>
  e instanceof ApiError && (e.status === 0 || e.status === 408 || e.status === 409 || e.status === 429 || e.status === 502 || e.status === 503 || e.status === 504)

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

/** PUTs one piece, reporting progress within it. */
function putPiece(url: string, blob: Blob, onProgress: (loaded: number) => void, track: (x: XMLHttpRequest) => void) {
  return new Promise<UploadProgress>((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    track(xhr)
    xhr.open('PUT', url)
    if (conn.token) xhr.setRequestHeader('Authorization', `Bearer ${conn.token}`)
    xhr.setRequestHeader('Content-Type', 'application/octet-stream')
    xhr.upload.onprogress = (e) => onProgress(e.loaded)
    xhr.onload = () => {
      let body: any = null
      try { body = JSON.parse(xhr.responseText) } catch { /* empty */ }
      if (xhr.status >= 200 && xhr.status < 300) resolve(body)
      else reject(new ApiError(xhr.status, body?.error?.code ?? 'error', body?.error?.message ?? `Upload failed (${xhr.status})`))
    }
    xhr.onerror = () => reject(new ApiError(0, 'unreachable', 'Connection lost during upload'))
    xhr.onabort = () => reject(new ApiError(0, 'cancelled', 'Cancelled'))
    xhr.send(blob)
  })
}

/**
 * Uploads a file, reporting progress. Large files go in pieces: when the
 * connection drops the upload asks how much arrived and continues from
 * there, and retrying (even after closing the app) picks up where it stopped.
 */
export function uploadFile(
  rootId: string,
  path: string,
  file: File,
  onProgress: (loaded: number) => void,
  prefix = '',
): { promise: Promise<FileDetails>; abort: () => void } {
  if (file.size <= CHUNK) return uploadWhole(rootId, path, file, onProgress, prefix)
  let aborted = false
  let current: XMLHttpRequest | null = null
  let uploadId: string | undefined
  const key = [prefix, rootId, path, file.name, file.size, file.lastModified].join('|')
  const base = `${prefix}/files/uploads`
  const cancelled = () => new ApiError(0, 'cancelled', 'Cancelled')

  /** How much the server has, trying again while it cannot be reached. */
  const offsetOf = async (id: string) => {
    for (let attempt = 1; ; attempt++) {
      try {
        return (await request<UploadProgress>('GET', `${base}/${id}`)).offset
      } catch (e) {
        if (aborted) throw cancelled()
        if (!retryable(e) || attempt > 8) throw e
        await sleep(Math.min(30_000, 1000 * 2 ** attempt))
      }
    }
  }

  const promise = (async () => {
    let id = resumable(key)
    let offset = 0
    if (id) {
      try { offset = await offsetOf(id) } catch { id = undefined }
    }
    if (!id) {
      const created = await request<UploadProgress>('POST', base, {
        root_id: rootId, path, name: file.name, size: file.size, modified_at: file.lastModified || undefined,
      })
      id = created.id
      offset = 0
      resumable(key, id)
    }
    uploadId = id
    onProgress(offset)
    let failures = 0
    while (offset < file.size) {
      if (aborted) throw cancelled()
      const start = offset
      const end = Math.min(file.size, start + CHUNK)
      try {
        const r = await putPiece(`${conn.baseUrl}/api/v1${base}/${id}?offset=${start}`, file.slice(start, end), (n) => onProgress(start + n), (x) => (current = x))
        offset = r.offset
        failures = 0
      } catch (e) {
        if (aborted || (e as ApiError).code === 'cancelled') throw cancelled()
        if (!retryable(e) || ++failures > 8) throw e
        await sleep(Math.min(30_000, 1000 * 2 ** failures))
        offset = await offsetOf(id)
      }
      onProgress(offset)
    }
    const done = await request<FileDetails>('POST', `${base}/${id}/complete`)
    resumable(key, null)
    return done
  })()
  return {
    promise,
    abort: () => {
      aborted = true
      current?.abort()
      // Cancelled on purpose: free the space the partial upload holds.
      if (uploadId) {
        resumable(key, null)
        void request<void>('DELETE', `${base}/${uploadId}`).catch(() => {})
      }
    },
  }
}

/** Uploads a small file in one request. */
function uploadWhole(
  rootId: string,
  path: string,
  file: File,
  onProgress: (loaded: number) => void,
  prefix: string,
): { promise: Promise<FileDetails>; abort: () => void } {
  const xhr = new XMLHttpRequest()
  const promise = new Promise<FileDetails>((resolve, reject) => {
    xhr.open('POST', `${conn.baseUrl}/api/v1${prefix}/files/upload${q({ root_id: rootId, path, name: file.name })}`)
    if (conn.token) xhr.setRequestHeader('Authorization', `Bearer ${conn.token}`)
    xhr.setRequestHeader('Content-Type', 'application/octet-stream')
    xhr.upload.onprogress = (e) => onProgress(e.loaded)
    xhr.onload = () => {
      let body: any = null
      try { body = JSON.parse(xhr.responseText) } catch { /* empty */ }
      if (xhr.status >= 200 && xhr.status < 300) resolve(body)
      else reject(new ApiError(xhr.status, body?.error?.code ?? 'error', body?.error?.message ?? `Upload failed (${xhr.status})`))
    }
    xhr.onerror = () => reject(new ApiError(0, 'unreachable', 'Connection lost during upload'))
    xhr.onabort = () => reject(new ApiError(0, 'cancelled', 'Cancelled'))
    xhr.send(file)
  })
  return { promise, abort: () => xhr.abort() }
}
