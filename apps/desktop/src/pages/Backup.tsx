import { useEffect, useState } from 'react'
import { ImageUp, Pause, Play, RefreshCw, ShieldAlert, Trash2 } from 'lucide-react'
import { androidBridge, api, peerPrefix, type BackupStatus, type Device, type Job } from '../services/api'
import { useResource } from '../hooks/useResource'
import { navigate, useRoute } from '../hooks/useRoute'
import { formatBytes, formatRelative } from '../lib/format'
import { toast, toastError } from '../components/Toasts'
import { Badge, Button, EmptyState, ErrorNote, Field, Modal, PageHeader, Panel, ProgressBar, Spinner } from '../components/ui'
import { PlacePicker } from './Sync'

/** Whether the app may read photos and videos; `null` outside the Android app. */
function useMediaAccess(): [boolean | null, () => void] {
  const bridge = androidBridge()
  const [granted, setGranted] = useState<boolean | null>(() => (bridge ? bridge.hasMediaAccess() : null))
  useEffect(() => {
    if (!bridge) return
    const check = () => setGranted(bridge.hasMediaAccess())
    window.addEventListener('nexus-media-access', check)
    document.addEventListener('visibilitychange', check)
    return () => {
      window.removeEventListener('nexus-media-access', check)
      document.removeEventListener('visibilitychange', check)
    }
  }, [bridge])
  return [granted, () => bridge?.requestMediaAccess()]
}

function AccessNote({ onAsk }: { onAsk: () => void }) {
  const [asked, setAsked] = useState(false)
  return (
    <Panel className="mb-4">
      <div className="flex items-start gap-3">
        <ShieldAlert className="mt-0.5 size-5 shrink-0 text-warn" />
        <div className="min-w-0 flex-1 space-y-2">
          <div className="font-medium">Allow HearthDrive to read your photos and videos</div>
          <p className="text-muted">Backup only reads them, to copy them to your computer. Nothing on the phone is changed or deleted.</p>
          <div className="flex flex-wrap gap-2">
            <Button
              variant="primary"
              size="sm"
              onClick={() => {
                setAsked(true)
                onAsk()
              }}
            >
              Allow access
            </Button>
            {asked && (
              <Button size="sm" variant="ghost" onClick={() => androidBridge()?.openAppSettings()}>
                Open app settings
              </Button>
            )}
          </div>
          {asked && <p className="text-[12px] text-subtle">If no question appeared, allow "Photos and videos" in the app's settings.</p>}
        </div>
      </div>
    </Panel>
  )
}

/** Sets up or changes where the backup goes. */
function Setup({ status, computers, onDone, onCancel }: {
  status: BackupStatus
  computers: Device[]
  onDone: () => void
  onCancel?: () => void
}) {
  const current = status.backup
  const [device, setDevice] = useState(current?.device_id ?? computers.find((d) => d.online)?.id ?? computers[0]?.id ?? '')
  const [place, setPlace] = useState({ root: current?.root_id ?? '', path: current?.path ?? '' })
  const [sources, setSources] = useState<Set<string>>(new Set(current?.sources ?? ['DCIM']))
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const online = computers.find((d) => d.id === device)?.online ?? false

  const save = async () => {
    setBusy(true)
    setError('')
    try {
      await api.setBackup({ device_id: device, root_id: place.root, path: place.path, sources: [...sources] })
      toast(current ? 'Backup changed' : 'Backup is on. The first run starts now.')
      onDone()
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Panel title={current ? 'Change backup' : 'Set up backup'} className="mb-4">
      <div className="space-y-4">
        <Field label="Back up">
          <div className="space-y-1.5">
            {status.choices.map((c) => (
              <label key={c.key} className="flex items-center gap-2.5">
                <input
                  type="checkbox"
                  className="accent-[var(--nx-accent)]"
                  checked={sources.has(c.key)}
                  onChange={(e) =>
                    setSources((prev) => {
                      const next = new Set(prev)
                      if (e.target.checked) next.add(c.key)
                      else next.delete(c.key)
                      return next
                    })
                  }
                />
                <span>{c.label}</span>
              </label>
            ))}
          </div>
        </Field>
        <Field label="To computer">
          <select
            value={device}
            onChange={(e) => {
              setDevice(e.target.value)
              setPlace({ root: '', path: '' })
            }}
            className="h-8 w-full rounded-md border border-line bg-bg px-2 text-fg outline-none focus:border-accent"
          >
            {computers.map((d) => (
              <option key={d.id} value={d.id}>{d.name}{d.online ? '' : ' (offline)'}</option>
            ))}
          </select>
        </Field>
        <Field label="Folder" hint="Photos keep their folders inside it, such as DCIM/Camera.">
          {online ? (
            <PlacePicker prefix={peerPrefix(device)} writable value={place} onChange={setPlace} />
          ) : (
            <p className="text-muted">This computer is offline. Open HearthDrive on it, then choose a folder.</p>
          )}
        </Field>
        <ErrorNote>{error}</ErrorNote>
        <div className="flex flex-wrap justify-end gap-2">
          {onCancel && <Button variant="ghost" onClick={onCancel}>Cancel</Button>}
          <Button variant="primary" loading={busy} disabled={!online || !place.root || sources.size === 0} onClick={() => void save()}>
            {current ? 'Save' : 'Turn on backup'}
          </Button>
        </div>
      </div>
    </Panel>
  )
}

export function Backup() {
  const status = useResource(() => api.backup(), [], (e) => e.type === 'sync.updated' || e.type === 'transfer.updated')
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.'))
  const [granted, askAccess] = useMediaAccess()
  const [editing, setEditing] = useState(false)
  const [removing, setRemoving] = useState(false)
  const [job, setJob] = useState<Job | null>(null)
  const running = status.data?.running ?? false

  // The "Back up now" home-screen shortcut opens this page with `run=1`.
  const route = useRoute()
  const runNow = route.params.get('run') === '1'
  useEffect(() => {
    if (!runNow) return
    navigate('backup')
    api.runBackup().then(() => toast('Backing up now')).catch(toastError)
  }, [runNow])

  // Progress of the run in progress, polled while it lasts.
  useEffect(() => {
    if (!running) {
      setJob(null)
      return
    }
    let stop = false
    const poll = async () => {
      try {
        const jobs = await api.jobs()
        if (!stop) setJob(jobs.find((j) => j.kind === 'backup' && (j.status === 'running' || j.status === 'queued')) ?? null)
      } catch {
        /* shown on the next poll */
      }
    }
    void poll()
    const t = window.setInterval(() => void poll(), 1000)
    return () => {
      stop = true
      window.clearInterval(t)
    }
  }, [running])

  const computers = (devices.data ?? []).filter((d) => !d.is_local)
  const b = status.data?.backup ?? null

  const act = async (f: () => Promise<unknown>, done?: string) => {
    try {
      await f()
      if (done) toast(done)
      status.reload()
    } catch (e) {
      toastError(e)
    }
  }

  let body
  if (status.error) body = <ErrorNote>{status.error.message}</ErrorNote>
  else if (!status.data || !devices.data) body = <Spinner />
  else if (!status.data.available)
    body = <EmptyState icon={<ImageUp className="size-6" />} title="Backup is for phones">Open HearthDrive on your phone to back up its photos and videos here.</EmptyState>
  else if (computers.length === 0)
    body = (
      <EmptyState
        icon={<ImageUp className="size-6" />}
        title="Pair a computer first"
        action={<Button variant="primary" onClick={() => navigate('devices')}>Go to Devices</Button>}
      >
        Photos and videos are backed up to one of your computers.
      </EmptyState>
    )
  else
    body = (
      <>
        {granted === false && <AccessNote onAsk={askAccess} />}
        {!b || editing ? (
          <Setup status={status.data} computers={computers} onDone={() => { setEditing(false); status.reload() }} onCancel={b ? () => setEditing(false) : undefined} />
        ) : (
          <Panel className="mb-4">
            <div className="space-y-3">
              <div className="flex flex-wrap items-center gap-2">
                <span className="font-medium">To {b.device_name}</span>
                <span className="text-muted">· {b.label}</span>
                {!b.enabled ? <Badge>Paused</Badge> : running ? <Badge tone="accent">Backing up</Badge> : b.last_error ? <Badge tone="warn">{b.last_result}</Badge> : <Badge tone="accent">On</Badge>}
              </div>
              <div className="text-[12.5px] text-muted">
                {status.data.choices.filter((c) => b.sources.includes(c.key)).map((c) => c.label).join(', ')}
              </div>
              {job && (
                <div className="space-y-1">
                  <ProgressBar value={job.bytes_total ? job.bytes_done / job.bytes_total : 0} />
                  <div className="flex justify-between gap-2 text-[12px] text-subtle">
                    <span className="truncate">{job.current ?? 'Getting ready…'}</span>
                    <span className="shrink-0 tabular">{job.files_done} of {job.files_total} · {formatBytes(job.bytes_done)} of {formatBytes(job.bytes_total)}</span>
                  </div>
                </div>
              )}
              {!running && (
                <div className="text-[12.5px]">
                  {b.last_run_at ? (
                    <>
                      <span className="text-muted">{b.last_result}</span>
                      <span className="text-subtle"> · {formatRelative(b.last_run_at)}</span>
                    </>
                  ) : (
                    <span className="text-subtle">Not run yet</span>
                  )}
                  {b.last_error && <ErrorNote>{b.last_error}</ErrorNote>}
                </div>
              )}
              <div className="flex flex-wrap gap-2">
                <Button size="sm" variant="primary" icon={<RefreshCw className="size-3.5" />} disabled={running || granted === false} onClick={() => void act(() => api.runBackup(), 'Backing up now')}>
                  Back up now
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  icon={b.enabled ? <Pause className="size-3.5" /> : <Play className="size-3.5" />}
                  onClick={() => void act(() => api.updateBackup({ enabled: !b.enabled }), b.enabled ? 'Backup paused' : 'Backup resumed')}
                >
                  {b.enabled ? 'Pause' : 'Resume'}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setEditing(true)}>Change</Button>
                <Button size="sm" variant="ghost" className="hover:text-danger" icon={<Trash2 className="size-3.5" />} onClick={() => setRemoving(true)}>
                  Turn off
                </Button>
              </div>
            </div>
          </Panel>
        )}
        <p className="text-[12px] text-subtle">
          Backup runs while HearthDrive is open on this phone: when you open it, every 15 minutes, and when you press Back up now. Only new and
          changed photos are sent. Deleting a photo here keeps its copy on the computer, and copies deleted there are not sent again.
        </p>
      </>
    )

  return (
    <div className="mx-auto max-w-3xl">
      <PageHeader title="Backup" subtitle="Copy photos and videos on this phone to a computer" />
      {body}
      <Modal
        open={removing}
        onClose={() => setRemoving(false)}
        title="Turn off backup?"
        footer={
          <>
            <Button variant="ghost" onClick={() => setRemoving(false)}>Cancel</Button>
            <Button
              variant="danger"
              onClick={() => {
                setRemoving(false)
                void act(() => api.removeBackup(), 'Backup turned off')
              }}
            >
              Turn off
            </Button>
          </>
        }
      >
        <p>Photos already copied stay on {b?.device_name ?? 'the computer'}. If you turn backup on again, it starts by checking what is already there.</p>
      </Modal>
    </div>
  )
}
