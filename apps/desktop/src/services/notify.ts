/**
 * System notifications: a finished or failed transfer, a device asking to
 * pair, a device that went offline, while the NEXUS window is not in front.
 */
import { api, isTauri, type Job } from './api'
import { subscribe } from './events'

const SETTING = 'nexus.notifications'

export type NotifyLevel = 'all' | 'important' | 'off'

export function notifyLevel(): NotifyLevel {
  try {
    const v = localStorage.getItem(SETTING)
    return v === 'all' || v === 'off' ? v : 'important'
  } catch {
    return 'important'
  }
}

export function setNotifyLevel(level: NotifyLevel) {
  try {
    localStorage.setItem(SETTING, level)
  } catch {
    // Not kept: notifications stay at the default.
  }
  if (level !== 'off') void allowed()
}

let permission: Promise<boolean> | null = null

async function allowed(): Promise<boolean> {
  if (!permission) {
    permission = (async () => {
      if (isTauri()) {
        const n = await import('@tauri-apps/plugin-notification')
        if (await n.isPermissionGranted()) return true
        return (await n.requestPermission()) === 'granted'
      }
      if (typeof Notification === 'undefined') return false
      if (Notification.permission === 'granted') return true
      return (await Notification.requestPermission()) === 'granted'
    })().catch(() => false)
  }
  return permission
}

async function show(title: string, body: string) {
  if (document.hasFocus() && !document.hidden) return
  if (!(await allowed())) return
  if (isTauri()) {
    const n = await import('@tauri-apps/plugin-notification')
    n.sendNotification({ title, body })
  } else {
    new Notification(title, { body })
  }
}

/** Starts turning events into notifications. Returns a stop function. */
let started = false

export function startNotifications(): () => void {
  if (started) return () => {}
  started = true
  const told = new Set<string>()
  const names = new Map<string, string>()
  const remember = () =>
    api.devices().then((list) => list.forEach((d) => names.set(d.id, d.name))).catch(() => {})
  void remember()

  const jobDone = async (id: string, status: string) => {
    const key = `${id}:${status}`
    if (told.has(key)) return
    told.add(key)
    const job: Job | undefined = await api.jobs().then((all) => all.find((j) => j.id === id)).catch(() => undefined)
    if (!job) return
    if (status === 'failed') {
      void show('Transfer failed', `${job.title}${job.errors[0] ? `: ${job.errors[0]}` : ''}`)
    } else if (notifyLevel() === 'all' || job.bytes_total > 100 * 1024 * 1024 || job.kind === 'backup') {
      void show(job.kind === 'backup' ? 'Backup finished' : 'Transfer finished', `${job.title} · ${job.route}`)
    }
  }

  return subscribe((e) => {
    const level = notifyLevel()
    if (level === 'off') return
    switch (e.type) {
      case 'transfer.updated':
        if (e.status === 'done' || e.status === 'failed') void jobDone(e.id, e.status)
        break
      case 'pairing.requested':
        void show('A device asks to pair', `${e.name} wants to join. Open Devices to compare fingerprints and approve it.`)
        break
      case 'device.changed':
        void remember()
        break
      case 'device.offline':
        if (level === 'all') void show('Device offline', `${names.get(e.device_id) ?? 'A paired device'} can no longer be reached.`)
        break
      case 'device.online':
        if (level === 'all' && names.has(e.device_id)) void show('Device online', `${names.get(e.device_id)} is back.`)
        break
    }
  })
}
