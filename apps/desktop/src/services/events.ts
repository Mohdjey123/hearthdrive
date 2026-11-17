// Live updates from nexus-service over a WebSocket.

import { connection } from './api'

export type NexusEvent =
  | { type: 'ready' | 'resync' }
  | { type: 'storage.changed'; root_id: string }
  | { type: 'storage.scan.started'; root_id: string }
  | { type: 'storage.scan.progress'; root_id: string; files_seen: number; bytes_seen: number }
  | { type: 'storage.scan.completed'; root_id: string; files: number; bytes: number; removed: number; duration_ms: number }
  | { type: 'storage.scan.failed'; root_id: string; error: string }
  | { type: 'file.created' | 'file.modified' | 'file.deleted'; root_id: string; path: string }
  | { type: 'file.moved'; root_id: string; from: string; to: string }
  | { type: 'device.changed' | 'device.offline' | 'device.removed'; device_id: string }
  | { type: 'device.online'; device_id: string; latency_ms: number }
  | { type: 'pairing.requested'; request_id: string; name: string }
  | { type: 'pairing.updated'; id: string; status: string }
  | { type: 'network.changed' }

type Handler = (event: NexusEvent) => void
type StatusHandler = (connected: boolean) => void

const handlers = new Set<Handler>()
const statusHandlers = new Set<StatusHandler>()
let socket: WebSocket | null = null
let retry = 0
let stopped = true
let connected = false

function setConnected(value: boolean) {
  connected = value
  statusHandlers.forEach((h) => h(value))
}

function open() {
  const { baseUrl, token } = connection()
  if (!token || stopped) return
  const url = baseUrl.replace(/^http/, 'ws') + '/api/v1/events'
  const ws = new WebSocket(url)
  socket = ws
  ws.onopen = () => ws.send(JSON.stringify({ type: 'auth', token }))
  ws.onmessage = (msg) => {
    let event: NexusEvent
    try { event = JSON.parse(msg.data) } catch { return }
    if (event.type === 'ready') {
      retry = 0
      setConnected(true)
    }
    handlers.forEach((h) => h(event))
  }
  ws.onclose = () => {
    socket = null
    setConnected(false)
    if (stopped) return
    const delay = Math.min(15000, 500 * 2 ** retry++)
    setTimeout(open, delay)
  }
}

export function startEvents() {
  if (!stopped) return
  stopped = false
  open()
}

export function stopEvents() {
  stopped = true
  socket?.close()
}

export function subscribe(handler: Handler): () => void {
  handlers.add(handler)
  return () => handlers.delete(handler)
}

export function subscribeStatus(handler: StatusHandler): () => void {
  statusHandlers.add(handler)
  handler(connected)
  return () => statusHandlers.delete(handler)
}
