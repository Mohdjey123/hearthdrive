// The node's jobs (copies, syncs, backups, checks), shared by every page that
// shows one. One request at a time for the whole app: refreshed when a job
// changes, and every second while one runs (progress is not pushed as events).

import { useEffect, useSyncExternalStore } from 'react'
import { api, type Job } from '../services/api'
import { subscribe } from '../services/events'
import { startActivity, toast } from '../components/Toasts'

let jobs: Job[] = []
let users = 0
let timer: ReturnType<typeof setInterval> | null = null
let unsubscribe: (() => void) | null = null
let loading = false
let again = false
const listeners = new Set<() => void>()

export const isActive = (j: Job) => j.status === 'queued' || j.status === 'running' || j.status === 'paused'

function load() {
  if (loading) {
    again = true
    return
  }
  loading = true
  api
    .jobs()
    .then((list) => {
      jobs = list
      listeners.forEach((l) => l())
      const running = list.some(isActive)
      if (running && !timer && users > 0) timer = setInterval(load, 1000)
      if (!running && timer) {
        clearInterval(timer)
        timer = null
      }
    })
    .catch(() => {})
    .finally(() => {
      loading = false
      if (again) {
        again = false
        load()
      }
    })
}

function start() {
  if (users++ > 0) return
  unsubscribe = subscribe((e) => {
    if (e.type === 'transfer.updated' || e.type === 'resync' || e.type === 'ready') load()
  })
  load()
}

function stop() {
  if (--users > 0) return
  unsubscribe?.()
  unsubscribe = null
  if (timer) clearInterval(timer)
  timer = null
}

/** Every job, kept fresh while the calling component is shown. */
export function useJobs(): Job[] {
  useEffect(() => {
    start()
    return stop
  }, [])
  return useSyncExternalStore(
    (l) => {
      listeners.add(l)
      return () => listeners.delete(l)
    },
    () => jobs,
  )
}

/** Asks for a fresh list now, e.g. right after starting a job. */
export const reloadJobs = () => load()

/** The running or waiting job about `subject` (a sync pair, backup set,
 *  storage root or file id), if there is one. */
export function useJobFor(subject: string | null | undefined): Job | undefined {
  const all = useJobs()
  return subject ? all.find((j) => j.subject === subject && isActive(j)) : undefined
}

/** A job by its id, while it is known (finished ones too). */
export function useJob(id: string | null | undefined): Job | undefined {
  const all = useJobs()
  return id ? all.find((j) => j.id === id) : undefined
}

/**
 * Shows a job's progress in a card at the bottom of the window until it
 * ends, wherever the person goes in the app; then a toast says how it went.
 * Hiding the card leaves the job running (it stays under Transfers).
 */
export function followJob(id: string, label: string) {
  let ended = false
  let seen = false
  const end = () => {
    if (ended) return
    ended = true
    clearTimeout(lost)
    listeners.delete(show)
    card.end()
    stop()
  }
  const card = startActivity(label, { noun: 'files', detail: 'Getting ready…', onHide: end })
  const show = () => {
    const job = jobs.find((j) => j.id === id)
    if (!job) return
    seen = true
    if (!isActive(job)) {
      end()
      const failed = job.errors.length
      if (job.status === 'done') toast(`${job.title}: done${failed ? `, ${failed} could not be` : ''}`, failed ? 'error' : 'success')
      else if (job.status === 'failed') toast(`${job.title} failed${job.errors[0] ? `: ${job.errors[0]}` : ''}`, 'error')
      return
    }
    card.update({
      done: job.files_done,
      total: job.files_total || undefined,
      bytesDone: job.bytes_done,
      bytesTotal: job.bytes_total || undefined,
      detail: job.status === 'running' ? job.current ?? 'Getting ready…' : job.note ?? (job.status === 'paused' ? 'Paused' : 'Waiting for its turn'),
    })
  }
  // A job this node does not list (it runs on another device) is not shown.
  const lost = setTimeout(() => {
    if (!seen) end()
  }, 15_000)
  listeners.add(show)
  start()
  load()
}
