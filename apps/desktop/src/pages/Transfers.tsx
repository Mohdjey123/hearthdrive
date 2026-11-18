import { useEffect, useState } from 'react'
import { ArrowUpDown, CheckCircle2, CopyPlus, FolderSync, ImageUp, Link2, RotateCcw, Upload, X, XCircle } from 'lucide-react'
import { cancelTransfer, clearFinished, retryTransfer, useTransfers, type Transfer } from '../stores/transfers'
import { api, type Job } from '../services/api'
import { useResource } from '../hooks/useResource'
import { formatBytes, formatDuration, formatRelative } from '../lib/format'
import { toastError } from '../components/Toasts'
import { Badge, Button, EmptyState, ErrorNote, IconButton, PageHeader, ProgressBar } from '../components/ui'

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
}
const JOB_ICONS = { copy: CopyPlus, sync: FolderSync, backup: ImageUp, share: Link2 }

function JobBadge({ job }: { job: Job }) {
  switch (job.status) {
    case 'queued': return <Badge>Waiting</Badge>
    case 'running': return <Badge tone="accent">{JOB_WORDS[job.kind]?.[0] ?? 'Running'}</Badge>
    case 'done':
      return job.errors.length ? <Badge tone="warn">Done, {job.errors.length} skipped</Badge> : <Badge tone="accent"><CheckCircle2 className="size-3" /> Done</Badge>
    case 'cancelled': return <Badge>Cancelled</Badge>
    case 'failed': return <Badge tone="danger"><XCircle className="size-3" /> Failed</Badge>
  }
}

const active = (j: Job) => j.status === 'queued' || j.status === 'running'

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
        <div className="w-8 shrink-0">
          {active(job) && (
            <IconButton label="Cancel" onClick={() => void api.cancelJob(job.id).then(onChanged).catch(toastError)}>
              <X className="size-4" />
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

  return (
    <div>
      <PageHeader
        title="Transfers"
        subtitle="Uploads, copies between devices and sync. Interrupted transfers continue where they stopped."
        actions={finished && <Button onClick={clear}>Clear finished</Button>}
      />
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
