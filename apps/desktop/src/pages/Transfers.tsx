import { useEffect, useState } from 'react'
import { ArrowUpDown, CheckCircle2, ChevronsUp, CopyPlus, FolderSync, ImageUp, Link2, Pause, Play, RotateCcw, ShieldCheck, Upload, X, XCircle } from 'lucide-react'
import { cancelTransfer, clearFinished, retryTransfer, useTransfers, type Transfer } from '../stores/transfers'
import { api, type Job, type TransferSettings } from '../services/api'
import { useResource } from '../hooks/useResource'
import { formatBytes, formatDuration, formatRelative } from '../lib/format'
import { toastError } from '../components/Toasts'
import { Badge, Button, EmptyState, ErrorNote, Field, IconButton, Input, Modal, PageHeader, ProgressBar } from '../components/ui'

function StatusBadge({ t }: { t: Transfer }) {
  switch (t.status) {
    case 'queued': return <Badge>Queued</Badge>
    case 'uploading': return <Badge tone="accent">Uploading</Badge>
    case 'done': return <Badge tone="accent"><CheckCircle2 className="size-3" /> Done</Badge>
    case 'cancelled': return <Badge>Cancelled</Badge>
    case 'error': return <Badge tone="danger"><XCircle className="size-3" /> Failed</Badge>
  }
}

/** What each kind of job is doing, and what it failed to do. */
const JOB_WORDS: Record<Job['kind'], [string, string]> = {
  copy: ['Copying', 'copied'],
  sync: ['Syncing', 'synced'],
  backup: ['Backing up', 'backed up'],
  share: ['Uploading', 'shared'],
  check: ['Checking', 'checked'],
}
const JOB_ICONS = { copy: CopyPlus, sync: FolderSync, backup: ImageUp, share: Link2, check: ShieldCheck }

function JobBadge({ job }: { job: Job }) {
  switch (job.status) {
    case 'queued': return <Badge>Waiting</Badge>
    case 'paused': return <Badge tone="warn">Paused</Badge>
    case 'running': return <Badge tone="accent">{JOB_WORDS[job.kind]?.[0] ?? 'Running'}</Badge>
    case 'done':
      return job.errors.length ? <Badge tone="warn">Done, {job.errors.length} skipped</Badge> : <Badge tone="accent"><CheckCircle2 className="size-3" /> Done</Badge>
    case 'cancelled': return <Badge>Cancelled</Badge>
    case 'failed': return <Badge tone="danger"><XCircle className="size-3" /> Failed</Badge>
  }
}

const active = (j: Job) => j.status === 'queued' || j.status === 'running' || j.status === 'paused'

/** Copies and syncs this device's node is running. */
function useJobs() {
  const jobs = useResource(() => api.jobs(), [], (e) => e.type === 'transfer.updated')
  const running = jobs.data?.some(active) ?? false
  // Progress is not pushed as events (too many); poll while something runs.
  useEffect(() => {
    if (!running) return
    const t = setInterval(jobs.reload, 1000)
    return () => clearInterval(t)
  }, [running, jobs.reload])
  return jobs
}

function JobRow({ job, onChanged }: { job: Job; onChanged: () => void }) {
  const [open, setOpen] = useState(false)
  const pct = job.bytes_total ? job.bytes_done / job.bytes_total : job.status === 'done' ? 1 : 0
  const Icon = JOB_ICONS[job.kind] ?? CopyPlus
  const elapsed = job.started_at ? Date.now() - job.started_at : 0
  const speed = elapsed > 2000 ? (job.bytes_done / elapsed) * 1000 : 0
  const remaining = speed > 0 ? ((job.bytes_total - job.bytes_done) / speed) * 1000 : 0
  return (
    <li className="px-4 py-3 md:px-5">
      <div className="flex items-center gap-3 md:gap-4">
        <Icon className="size-4 shrink-0 text-muted" />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate font-medium">{job.title}</span>
            <JobBadge job={job} />
          </div>
          <div className="mt-0.5 truncate text-[12px] text-subtle">
            {job.route}
            {job.status === 'running' && job.current && ` · ${job.current}`}
            {job.finished_at && ` · ${formatRelative(job.finished_at)}`}
            {job.retries > 0 && ` · reconnected ${job.retries}×`}
            {job.verified > 0 && ` · ${job.verified} checked`}
            {job.note && ` · ${job.note}`}
          </div>
          {active(job) && <ProgressBar className="mt-2" value={pct} />}
        </div>
        <div className="w-24 shrink-0 text-right text-[12px] text-muted tabular md:w-44">
          <div>{job.files_done} / {job.files_total} files</div>
          <div className="text-subtle">
            {formatBytes(job.bytes_done)} / {formatBytes(job.bytes_total)}
            {job.status === 'running' && speed > 0 && <span className="hidden md:inline"> · {formatDuration(remaining)} left</span>}
          </div>
        </div>
        <div className="flex shrink-0 gap-0.5">
          {job.status === 'queued' && (
            <IconButton label="Run next" onClick={() => void api.runJobNext(job.id).then(onChanged).catch(toastError)}>
              <ChevronsUp className="size-4" />
            </IconButton>
          )}
          {(job.status === 'running' || job.status === 'queued') && (
            <IconButton label="Pause" onClick={() => void api.pauseJob(job.id).then(onChanged).catch(toastError)}>
              <Pause className="size-4" />
            </IconButton>
          )}
          {job.status === 'paused' && (
            <IconButton label="Resume" onClick={() => void api.resumeJob(job.id).then(onChanged).catch(toastError)}>
              <Play className="size-4" />
            </IconButton>
          )}
          {active(job) && (
            <IconButton label="Cancel" onClick={() => void api.cancelJob(job.id).then(onChanged).catch(toastError)}>
              <X className="size-4" />
            </IconButton>
          )}
          {job.can_retry && (
            <IconButton label="Retry what failed" onClick={() => void api.retryJob(job.id).then(onChanged).catch(toastError)}>
              <RotateCcw className="size-4" />
            </IconButton>
          )}
        </div>
      </div>
      {job.errors.length > 0 && (
        <div className="mt-2 pl-7">
          <button className="text-[12px] text-accent hover:underline" onClick={() => setOpen((v) => !v)}>
            {open ? 'Hide' : 'Show'} what could not be {JOB_WORDS[job.kind]?.[1] ?? 'copied'} ({job.errors.length})
          </button>
          {open && (
            <ul className="mt-1 max-h-40 space-y-0.5 overflow-auto text-[12px] text-danger">
              {job.errors.map((e, i) => <li key={i} className="break-words">{e}</li>)}
            </ul>
          )}
        </div>
      )}
    </li>
  )
}

/** How many transfers run at once, how fast, and when. */
function SettingsDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [form, setForm] = useState<TransferSettings | null>(null)
  const [canConfigure, setCanConfigure] = useState(false)
  const [limited, setLimited] = useState(false)
  const [scheduled, setScheduled] = useState(false)
  const [saving, setSaving] = useState(false)

  useEffect(() => {
    if (!open) return
    api
      .transferSettings()
      .then(({ can_configure, ...s }) => {
        setForm(s)
        setCanConfigure(can_configure)
        setLimited(s.limit_kbps > 0)
        setScheduled(Boolean(s.hours_start && s.hours_end))
      })
      .catch(toastError)
  }, [open])

  const save = async () => {
    if (!form) return
    setSaving(true)
    try {
      await api.setTransferSettings({
        parallel: form.parallel,
        limit_kbps: limited ? Math.max(1, Math.round(form.limit_kbps || 1024)) : 0,
        hours_start: scheduled ? form.hours_start || '22:00' : null,
        hours_end: scheduled ? form.hours_end || '07:00' : null,
      })
      onClose()
    } catch (e) {
      toastError(e)
    } finally {
      setSaving(false)
    }
  }

  return (
    <Modal
      title="Transfer settings"
      open={open}
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={saving} disabled={!form || !canConfigure} onClick={() => void save()}>Save</Button>
        </>
      }
    >
      {!form ? null : (
        <fieldset disabled={!canConfigure} className="space-y-4">
          {!canConfigure && <p className="text-[12.5px] text-subtle">Only an owner or administrator can change these.</p>}
          <Field label="Transfers at the same time" hint="The others wait their turn. Run next puts one first in line.">
            <select
              className="h-8 rounded-md border border-line bg-panel px-2 text-[13px]"
              value={form.parallel}
              onChange={(e) => setForm({ ...form, parallel: Number(e.target.value) })}
            >
              {[1, 2, 3, 4].map((n) => <option key={n} value={n}>{n}</option>)}
            </select>
          </Field>
          <div className="space-y-1.5">
            <label className="flex items-center gap-2">
              <input type="checkbox" className="accent-[var(--nx-accent)]" checked={limited} onChange={(e) => setLimited(e.target.checked)} />
              <span>Limit the speed</span>
            </label>
            {limited && (
              <div className="flex items-center gap-2 pl-6">
                <Input
                  type="number"
                  min={1}
                  className="w-28"
                  value={form.limit_kbps || 1024}
                  onChange={(e) => setForm({ ...form, limit_kbps: Number(e.target.value) })}
                />
                <span className="text-muted">KB/s for all copies, syncs and backups together</span>
              </div>
            )}
          </div>
          <div className="space-y-1.5">
            <label className="flex items-center gap-2">
              <input type="checkbox" className="accent-[var(--nx-accent)]" checked={scheduled} onChange={(e) => setScheduled(e.target.checked)} />
              <span>Only transfer between certain hours</span>
            </label>
            {scheduled && (
              <div className="flex flex-wrap items-center gap-2 pl-6">
                <Input type="time" className="w-28" value={form.hours_start ?? '22:00'} onChange={(e) => setForm({ ...form, hours_start: e.target.value })} />
                <span className="text-muted">to</span>
                <Input type="time" className="w-28" value={form.hours_end ?? '07:00'} onChange={(e) => setForm({ ...form, hours_end: e.target.value })} />
                <span className="block w-full text-[12px] text-subtle">
                  This computer's time. Outside these hours transfers wait, and running ones pause and continue later.
                </span>
              </div>
            )}
          </div>
        </fieldset>
      )}
    </Modal>
  )
}

export function Transfers() {
  const transfers = useTransfers()
  const jobs = useJobs()
  const jobList = jobs.data ?? []
  const finished =
    transfers.some((t) => t.status === 'done' || t.status === 'error' || t.status === 'cancelled') || jobList.some((j) => !active(j))
  const clear = () => {
    clearFinished()
    void api.clearJobs().then(jobs.reload).catch(toastError)
  }
  const empty = transfers.length === 0 && jobList.length === 0
  const [settingsOpen, setSettingsOpen] = useState(false)

  return (
    <div>
      <PageHeader
        title="Transfers"
        subtitle="Uploads, copies between devices and sync. Interrupted transfers continue where they stopped."
        actions={
          <>
            {finished && <Button onClick={clear}>Clear finished</Button>}
            <Button variant="ghost" onClick={() => setSettingsOpen(true)}>Settings</Button>
          </>
        }
      />
      <SettingsDialog open={settingsOpen} onClose={() => setSettingsOpen(false)} />
      {jobs.error && <div className="mb-3"><ErrorNote>{jobs.error.message}</ErrorNote></div>}
      <div className="rounded-lg border border-line bg-panel">
        {empty ? (
          <EmptyState icon={<ArrowUpDown className="size-8" />} title="No transfers">
            Uploads, copies between devices and sync runs appear here with their progress.
          </EmptyState>
        ) : (
          <ul className="divide-y divide-line">
            {jobList.map((j) => <JobRow key={j.id} job={j} onChanged={jobs.reload} />)}
            {transfers.map((t) => {
              const pct = t.size ? t.loaded / t.size : t.status === 'done' ? 1 : 0
              const remaining = t.speed > 0 ? ((t.size - t.loaded) / t.speed) * 1000 : 0
              return (
                <li key={t.id} className="flex items-center gap-3 px-4 py-3 md:gap-4 md:px-5">
                  <Upload className="size-4 shrink-0 text-muted" />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="truncate font-medium">{t.name}</span>
                      <StatusBadge t={t} />
                    </div>
                    <div className="mt-0.5 truncate text-[12px] text-subtle">
                      to {t.rootName}{t.path ? ` / ${t.path}` : ''}
                      {t.error && <span className="text-danger"> · {t.error}</span>}
                    </div>
                    {(t.status === 'uploading' || t.status === 'queued') && (
                      <ProgressBar className="mt-2" value={pct} />
                    )}
                  </div>
                  <div className="w-24 shrink-0 text-right text-[12px] text-muted tabular md:w-44">
                    <div>{formatBytes(t.loaded)} / {formatBytes(t.size)}</div>
                    {t.status === 'uploading' && t.speed > 0 && (
                      <div className="text-subtle">{formatBytes(t.speed)}/s · {formatDuration(remaining)} left</div>
                    )}
                  </div>
                  <div className="w-8 shrink-0">
                    {(t.status === 'uploading' || t.status === 'queued') && (
                      <IconButton label="Cancel" onClick={() => cancelTransfer(t.id)}><X className="size-4" /></IconButton>
                    )}
                    {(t.status === 'error' || t.status === 'cancelled') && (
                      <IconButton label="Retry" onClick={() => retryTransfer(t.id)}><RotateCcw className="size-4" /></IconButton>
                    )}
                  </div>
                </li>
              )
            })}
          </ul>
        )}
      </div>
    </div>
  )
}
