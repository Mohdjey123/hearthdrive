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
  roots: number
  indexed_bytes: number
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

export interface User { id: string; username: string; role: Role; created_at: number }
export interface Me { user_id: string | null; username: string; role: Role; local: boolean; device_id: string | null }
export interface Session { token: string; expires_at: number; user: User }

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

interface Connection { baseUrl: string; token: string | null; local: boolean; dataDir?: string }

const TOKEN_KEY = 'nexus.session'
let conn: Connection = { baseUrl: '', token: null, local: false }
let unauthorizedHandler: (() => void) | null = null

export const isTauri = () => typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

/** Resolves the service address: from the Tauri shell, or the page origin. */
export async function initConnection(): Promise<Connection> {
  if (isTauri()) {
    const { invoke } = await import('@tauri-apps/api/core')
    const c = await invoke<{ base_url: string; token: string; data_dir: string }>('service_connection')
    conn = { baseUrl: c.base_url, token: c.token, local: true, dataDir: c.data_dir }
  } else {
    let stored: string | null = null
    try { stored = localStorage.getItem(TOKEN_KEY) } catch { /* storage unavailable */ }
    const base = (import.meta.env.VITE_NEXUS_API as string | undefined) ?? window.location.origin
    conn = { baseUrl: base.replace(/\/$/, ''), token: stored, local: false }
  }
  return conn
}

export const connection = () => conn

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
    if (res.status === 401 && path !== '/auth/login') unauthorizedHandler?.()
    throw new ApiError(res.status, code, message)
  }
  if (res.status === 204 || res.status === 202) return undefined as T
  return res.json() as Promise<T>
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
  login: (username: string, password: string) => request<Session>('POST', '/auth/login', { username, password }),
  logout: () => request<void>('POST', '/auth/logout'),
  me: () => request<Me>('GET', '/auth/me'),
  activity: (limit = 20) => request<ActivityEntry[]>('GET', `/activity${q({ limit })}`),

  drives: () => request<Drive[]>('GET', '/storage/drives'),
  addRoot: (b: { path: string; name?: string; read_only?: boolean }) => request<StorageRoot>('POST', '/storage/roots', b),
  removeRoot: (id: string) => request<void>('DELETE', `/storage/roots/${id}`),
  rescanRoot: (id: string) => request<void>('POST', `/storage/roots/${id}/scan`),

  devices: () => request<Device[]>('GET', '/devices'),
  updateDevice: (id: string, b: { name?: string; role?: Role }) => request<void>('PATCH', `/devices/${id}`, b),
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

/** Uploads a file as a raw streamed body, reporting progress. */
export function uploadFile(
  rootId: string,
  path: string,
  file: File,
  onProgress: (loaded: number) => void,
  prefix = '',
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
