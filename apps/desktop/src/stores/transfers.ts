// Client-side upload queue. Uploads stream straight to the destination
// volume; progress, speed, cancel and retry are tracked here.

import { useSyncExternalStore } from 'react'
import { uploadFile } from '../services/api'

export type TransferStatus = 'queued' | 'uploading' | 'done' | 'error' | 'cancelled'

export interface Transfer {
  id: string
  name: string
  size: number
  loaded: number
  status: TransferStatus
  error?: string
  rootId: string
  rootName: string
  path: string
  /** Proxy prefix when uploading to a paired device; '' for this computer. */
  prefix: string
  startedAt?: number
  finishedAt?: number
  speed: number
  file: File
}

const CONCURRENCY = 2
let transfers: Transfer[] = []
const listeners = new Set<() => void>()
const aborters = new Map<string, () => void>()
const completionListeners = new Set<(t: Transfer) => void>()

function emit() {
  transfers = [...transfers]
  listeners.forEach((l) => l())
}

function patch(id: string, changes: Partial<Transfer>) {
  const t = transfers.find((x) => x.id === id)
  if (t) Object.assign(t, changes)
}

function pump() {
  while (transfers.filter((t) => t.status === 'uploading').length < CONCURRENCY) {
    const next = transfers.find((t) => t.status === 'queued')
    if (!next) break
    start(next)
  }
  emit()
}

function start(t: Transfer) {
  t.status = 'uploading'
  t.startedAt = Date.now()
  t.loaded = 0
  let lastLoaded = 0
  let lastTime = Date.now()
  let lastEmit = 0
  const { promise, abort } = uploadFile(t.rootId, t.path, t.file, (loaded) => {
    const now = Date.now()
    if (now - lastTime >= 500) {
      const instant = ((loaded - lastLoaded) / (now - lastTime)) * 1000
      t.speed = t.speed ? t.speed * 0.6 + instant * 0.4 : instant
      lastLoaded = loaded
      lastTime = now
    }
    t.loaded = loaded
    if (now - lastEmit > 120) {
      lastEmit = now
      emit()
    }
  }, t.prefix)
  aborters.set(t.id, abort)
  promise
    .then(() => {
      patch(t.id, { status: 'done', loaded: t.size, finishedAt: Date.now() })
      completionListeners.forEach((l) => l(t))
    })
    .catch((e: Error & { code?: string }) => {
      if (e.code === 'cancelled') patch(t.id, { status: 'cancelled', finishedAt: Date.now() })
      else patch(t.id, { status: 'error', error: e.message, finishedAt: Date.now() })
    })
    .finally(() => {
      aborters.delete(t.id)
      pump()
    })
}

export function enqueueUploads(rootId: string, rootName: string, path: string, files: File[], prefix = '') {
  for (const file of files) {
    transfers.unshift({
      id: crypto.randomUUID(),
      name: file.name,
      size: file.size,
      loaded: 0,
      status: 'queued',
      rootId,
      rootName,
      path,
      prefix,
      speed: 0,
      file,
    })
  }
  pump()
}

export function cancelTransfer(id: string) {
  const t = transfers.find((x) => x.id === id)
  if (!t) return
  if (t.status === 'queued') patch(id, { status: 'cancelled' })
  aborters.get(id)?.()
  emit()
}

export function retryTransfer(id: string) {
  const t = transfers.find((x) => x.id === id)
  if (!t || (t.status !== 'error' && t.status !== 'cancelled')) return
  patch(id, { status: 'queued', error: undefined, loaded: 0, speed: 0 })
  pump()
}

export function clearFinished() {
  transfers = transfers.filter((t) => t.status === 'queued' || t.status === 'uploading')
  emit()
}

export function onUploadComplete(listener: (t: Transfer) => void) {
  completionListeners.add(listener)
  return () => completionListeners.delete(listener)
}

export function useTransfers(): Transfer[] {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb)
      return () => listeners.delete(cb)
    },
    () => transfers,
  )
}
