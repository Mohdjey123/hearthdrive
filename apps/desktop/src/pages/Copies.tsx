import { useState } from 'react'
import { ArchiveRestore, Copy, Pause, Play, RefreshCw, ShieldCheck, Trash2 } from 'lucide-react'
import { api, peerPrefix, type Device, type HeldFile, type SyncPair } from '../services/api'
import { useResource } from '../hooks/useResource'
import { formatBytes, formatDate, formatRelative } from '../lib/format'
import { toast, toastError } from '../components/Toasts'
import { Badge, Button, EmptyState, ErrorNote, Field, IconButton, Modal, PageHeader, Spinner, StatusDot, cx } from '../components/ui'
import { PlacePicker } from './Sync'

const DAY = 24 * 60 * 60 * 1000
const KEEP: { days: number; label: string }[] = [
  { days: 7, label: '7 days' },
  { days: 30, label: '30 days' },
  { days: 90, label: '90 days' },
  { days: 365, label: '1 year' },
  { days: 0, label: 'Do not keep' },
]

/** A copy that is up to date and can be relied on. */
function inStep(p: SyncPair) {
  return p.enabled && !p.last_error && !!p.last_ok_at && !p.running
}

function status(p: SyncPair): { tone: 'online' | 'offline' | 'busy' | 'error'; text: string } {
  if (p.running) return { tone: 'busy', text: 'Copying now…' }
  if (p.checking) return { tone: 'busy', text: 'Reading the copy back…' }
  if (!p.enabled) return { tone: 'offline', text: 'Paused' }
  if (p.last_error) return { tone: p.last_result === 'Waiting for the device' ? 'offline' : 'error', text: p.last_result ?? 'Needs attention' }
  if (!p.last_run_at) return { tone: 'busy', text: 'Starting…' }
  return { tone: 'online', text: `${p.last_result ?? 'Up to date'} · ${formatRelative(p.last_ok_at ?? p.last_run_at)}` }
}

function KeepSelect({ value, onChange, disabled }: { value: number; onChange: (days: number) => void; disabled?: boolean }) {
  const options = KEEP.some((k) => k.days === value) ? KEEP : [...KEEP, { days: value, label: `${value} days` }]
  return (
    <select
      value={value}
      disabled={disabled}
      onChange={(e) => onChange(Number(e.target.value))}
      aria-label="Keep files removed here"
      className="h-7 rounded-md border border-line bg-bg px-1.5 text-[12.5px] text-fg outline-none focus:border-accent"
    >
      {options.map((k) => <option key={k.days} value={k.days}>{k.label}</option>)}
    </select>
  )
}

function KeptFiles({ copy, onClose }: { copy: SyncPair; onClose: () => void }) {
  const files = useResource(() => api.heldFiles(copy.id), [copy.id])
  const [picked, setPicked] = useState<Set<string>>(new Set())
  const [busy, setBusy] = useState(false)
  const list = files.data ?? []
  const toggle = (path: string) =>
    setPicked((s) => {
      const n = new Set(s)
      if (n.has(path)) n.delete(path)
      else n.add(path)
      return n
    })
  const bringBack = async () => {
    setBusy(true)
    try {
      await api.bringBack(copy.id, [...picked])
      toast(`Bringing back ${picked.size} item${picked.size === 1 ? '' : 's'} from ${copy.device_name}. Follow it in Transfers.`)
      onClose()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal
      open
      wide
      onClose={onClose}
      title={`Removed here, kept on ${copy.device_name}`}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Close</Button>
          <Button variant="primary" loading={busy} disabled={picked.size === 0} onClick={() => void bringBack()}>
            <ArchiveRestore className="size-4" /> Bring back{picked.size ? ` (${picked.size})` : ''}
          </Button>
        </>
      }
    >
      {files.error && <ErrorNote>{files.error.message}</ErrorNote>}
      {!files.data ? (
        <Spinner />
      ) : list.length === 0 ? (
        <p className="text-muted">Nothing removed here is being kept on the copy.</p>
      ) : (
        <>
          <p className="mb-3 text-[12.5px] text-subtle">
            These were removed from {copy.local_label} but stay on {copy.device_name} until the date shown, then go to its trash.
            Bringing them back copies them here under their old names.
          </p>
          <ul className="max-h-[50vh] divide-y divide-line overflow-auto rounded-md border border-line">
            {list.map((f: HeldFile) => (
              <li key={f.path}>
                <label className="flex cursor-pointer items-center gap-3 px-3 py-2 hover:bg-hover">
                  <input type="checkbox" className="accent-[var(--nx-accent)]" checked={picked.has(f.path)} onChange={() => toggle(f.path)} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate font-medium" title={f.path}>{f.path}{f.is_dir ? '/' : ''}</span>
                    <span className="block text-[12px] text-subtle">
                      {formatBytes(f.size)} · removed {formatRelative(f.gone_at)} · kept until {formatDate(f.gone_at + copy.keep_days * DAY)}
                    </span>
                  </span>
                </label>
              </li>
            ))}
          </ul>
        </>
      )}
    </Modal>
  )
}

function CopyRow({ copy, onChanged }: { copy: SyncPair; onChanged: () => void }) {
  const [busy, setBusy] = useState(false)
  const [removing, setRemoving] = useState(false)
  const [showKept, setShowKept] = useState(false)
  const st = status(copy)
  const act = async (fn: () => Promise<unknown>, done?: string) => {
    setBusy(true)
    try {
      await fn()
      if (done) toast(done)
      onChanged()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  return (
    <li className="px-4 py-3">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <div className="min-w-0 flex-1">
          <div className="truncate font-medium" title={copy.remote_label}>
            {copy.device_name} <span className="font-normal text-muted">· {copy.remote_label}</span>
          </div>
          <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-[12.5px]">
            <StatusDot tone={st.tone} />
            <span className="truncate">{st.text}</span>
          </div>
        </div>
        <span className="flex flex-wrap items-center gap-1">
          <Button size="sm" disabled={busy || copy.running} onClick={() => void act(() => api.runSync(copy.id), 'Bringing the copy up to date')}>
            <RefreshCw className={cx('size-3.5', copy.running && 'animate-spin')} /> Copy now
          </Button>
          <Button
            size="sm"
            disabled={busy || copy.checking || !copy.last_ok_at}
            onClick={() => void act(() => api.checkCopy(copy.id), `Reading the copy on ${copy.device_name} back`)}
          >
            <ShieldCheck className="size-3.5" /> Check
          </Button>
          <IconButton
            label={copy.enabled ? 'Pause' : 'Resume'}
            disabled={busy}
            onClick={() => void act(() => api.updateSync(copy.id, { enabled: !copy.enabled }), copy.enabled ? 'Copy paused' : 'Copy resumed')}
          >
            {copy.enabled ? <Pause className="size-4" /> : <Play className="size-4" />}
          </IconButton>
          <IconButton label="Stop keeping this copy" className="hover:text-danger" onClick={() => setRemoving(true)}>
            <Trash2 className="size-4" />
          </IconButton>
        </span>
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1.5 text-[12.5px] text-muted">
        <span>
          {copy.last_check_at ? (
            <>Checked {formatRelative(copy.last_check_at)}: {copy.last_check_result}</>
          ) : (
            'Not read back yet'
          )}
        </span>
        <span className="flex items-center gap-1.5">
          Keep files removed here:
          <KeepSelect
            value={copy.keep_days}
            disabled={busy}
            onChange={(keep_days) => void act(() => api.updateSync(copy.id, { keep_days }), 'Saved')}
          />
        </span>
        {copy.held_files > 0 && (
          <button className="text-accent hover:underline" onClick={() => setShowKept(true)}>
            {copy.held_files} removed here {copy.held_files === 1 ? 'is' : 'are'} kept ({formatBytes(copy.held_bytes)})
          </button>
        )}
      </div>
      {copy.last_error && <p className="mt-2 text-[12px] break-words text-danger">{copy.last_error}</p>}
      {showKept && <KeptFiles copy={copy} onClose={() => setShowKept(false)} />}
      <Modal
        open={removing}
        onClose={() => setRemoving(false)}
        title={`Stop keeping a copy on ${copy.device_name}?`}
        footer={
          <>
            <Button variant="ghost" onClick={() => setRemoving(false)}>Keep it</Button>
            <Button variant="danger" loading={busy} onClick={() => void act(() => api.removeSync(copy.id), 'Copy removed').then(() => setRemoving(false))}>
              Stop keeping it
            </Button>
          </>
        }
      >
        <p>
          The files already on {copy.device_name} ({copy.remote_label}) stay there; they just stop being kept up to date.
          {copy.held_files > 0 && ` The ${copy.held_files} removed here that it kept stay there too.`}
        </p>
      </Modal>
    </li>
  )
}

function NewCopy({ devices, preset, onClose, onCreated }: {
  devices: Device[]
  preset?: { root: string; path: string; label: string }
  onClose: () => void
  onCreated: () => void
}) {
  const online = devices.filter((d) => d.online)
  const [device, setDevice] = useState(online[0]?.id ?? devices[0]?.id ?? '')
  const [local, setLocal] = useState(preset ? { root: preset.root, path: preset.path } : { root: '', path: '' })
  const [remote, setRemote] = useState({ root: '', path: '' })
  const [keep, setKeep] = useState(30)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const deviceName = devices.find((d) => d.id === device)?.name ?? 'the device'

  const create = async () => {
    setBusy(true)
    setError('')
    try {
      await api.createSync({
        local_root_id: local.root,
        local_path: local.path,
        device_id: device,
        remote_root_id: remote.root,
        remote_path: remote.path,
        mode: 'copy',
        keep_days: keep,
      })
      toast(`Copy set up. Copying to ${deviceName} starts now.`)
      onCreated()
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      open
      wide
      onClose={onClose}
      title={preset ? `Another copy of ${preset.label}` : 'Keep a copy of a folder on another device'}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!local.root || !remote.root || !device} onClick={() => void create()}>
            Keep a copy
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <Field label="Folder on this computer">
            {preset ? <p className="font-medium">{preset.label}</p> : <PlacePicker prefix="" writable={false} value={local} onChange={setLocal} />}
          </Field>
          <div className="space-y-2">
            <Field label="Device">
              <select
                value={device}
                onChange={(e) => { setDevice(e.target.value); setRemote({ root: '', path: '' }) }}
                className="h-8 w-full rounded-md border border-line bg-bg px-2 text-fg outline-none focus:border-accent"
              >
                {devices.map((d) => (
                  <option key={d.id} value={d.id} disabled={!d.online}>{d.name}{d.online ? '' : ' (offline)'}</option>
                ))}
              </select>
            </Field>
            {device && (
              <Field label={`Where on ${deviceName}`} hint="Choose an empty folder, or one meant for this copy.">
                <PlacePicker key={device} prefix={peerPrefix(device)} writable value={remote} onChange={setRemote} />
              </Field>
            )}
          </div>
        </div>
        <Field label="Files removed here">
          <span className="flex items-center gap-2 text-[12.5px] text-muted">
            stay on the copy for <KeepSelect value={keep} onChange={setKeep} /> and can be brought back, then go to its trash.
          </span>
        </Field>
        <p className="text-[12px] text-subtle">
          The copy is an ordinary folder you can open on {deviceName}. NEXUS keeps it the same as the folder here: new and changed files are
          copied over and checked, a file changed on {deviceName} is replaced by the one here (its old content is kept as a version), and once a
          week part of the copy is read back to make sure it is still intact. It keeps no history: for earlier versions, also set up a backup.
        </p>
        {error && <ErrorNote>{error}</ErrorNote>}
      </div>
    </Modal>
  )
}

interface Group {
  key: string
  root: string
  path: string
  label: string
  copies: SyncPair[]
}

export function Copies() {
  const pairs = useResource(() => api.syncPairs(), [], (e) => e.type === 'sync.updated' || e.type === 'device.online' || e.type === 'device.offline', 'copies')
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.'), 'devices')
  const peers = (devices.data ?? []).filter((d) => !d.is_local && d.public_key)
  const [adding, setAdding] = useState<null | { preset?: Group }>(null)

  const groups: Group[] = []
  for (const p of (pairs.data ?? []).filter((p) => p.mode === 'copy')) {
    const key = `${p.local_root_id}:${p.local_path.toLowerCase()}`
    let g = groups.find((g) => g.key === key)
    if (!g) {
      g = { key, root: p.local_root_id, path: p.local_path, label: p.local_label, copies: [] }
      groups.push(g)
    }
    g.copies.push(p)
  }

  return (
    <div>
      <PageHeader
        title="Copies"
        subtitle="Keep folders on more than one computer, so one failing drive does not lose them"
        actions={peers.length > 0 && <Button variant="primary" onClick={() => setAdding({})}><Copy className="size-4" /> Keep a copy</Button>}
      />
      {pairs.error && <div className="mb-3"><ErrorNote>{pairs.error.message}</ErrorNote></div>}
      {!pairs.data ? (
        <Spinner />
      ) : groups.length === 0 ? (
        <div className="rounded-lg border border-line bg-panel">
          <EmptyState
            icon={<Copy className="size-8" />}
            title="No folder has an extra copy yet"
            action={peers.length > 0 ? <Button variant="primary" onClick={() => setAdding({})}>Keep a copy of a folder</Button> : undefined}
          >
            {peers.length > 0
              ? 'Pick a folder here and a place on another computer. NEXUS keeps a checked, ordinary copy there; add more computers for three copies.'
              : 'Pair another computer first (Devices → Add a device), then keep copies of folders on it.'}
          </EmptyState>
        </div>
      ) : (
        <ul className="space-y-3">
          {groups.map((g) => {
            const good = g.copies.filter(inStep)
            const total = 1 + good.length
            return (
              <li key={g.key} className="rounded-lg border border-line bg-panel">
                <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-3">
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-medium" title={g.label}>{g.label}</div>
                    <div className="text-[12.5px] text-muted">
                      On this computer{good.length ? ` and ${good.map((c) => c.device_name).join(', ')}` : ''}
                    </div>
                  </div>
                  <Badge tone={total >= 2 ? 'accent' : 'warn'}>{total} {total === 1 ? 'copy' : 'copies'}</Badge>
                  {peers.length > g.copies.length && (
                    <Button size="sm" variant="ghost" onClick={() => setAdding({ preset: g })}>Add another</Button>
                  )}
                </div>
                <ul className="divide-y divide-line">
                  {g.copies.map((c) => <CopyRow key={c.id} copy={c} onChanged={pairs.reload} />)}
                </ul>
              </li>
            )
          })}
        </ul>
      )}
      {adding && (
        <NewCopy
          devices={peers.filter((d) => !adding.preset?.copies.some((c) => c.device_id === d.id))}
          preset={adding.preset && { root: adding.preset.root, path: adding.preset.path, label: adding.preset.label }}
          onClose={() => setAdding(null)}
          onCreated={() => {
            setAdding(null)
            pairs.reload()
          }}
        />
      )}
    </div>
  )
}
