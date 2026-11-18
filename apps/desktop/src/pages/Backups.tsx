import { useState } from 'react'
import { ArrowRight, ChevronRight, Copy, File, Folder, FolderOpen, History, KeyRound, Lock, Pause, Play, RefreshCw, RotateCcw, Settings2, ShieldCheck, Trash2 } from 'lucide-react'
import { api, type BackupSchedule, type Device, type SnapshotSet } from '../services/api'
import { useResource } from '../hooks/useResource'
import { formatBytes, formatRelative } from '../lib/format'
import { PlacePicker } from './Sync'
import { toast, toastError } from '../components/Toasts'
import { ConfirmFields } from '../components/Security'
import { Badge, Button, EmptyState, ErrorNote, Field, IconButton, Input, Modal, PageHeader, Spinner, StatusDot, cx } from '../components/ui'

const DEFAULT_SCHEDULE: BackupSchedule = { every_hours: 24, keep_last: 7, keep_daily: 14, keep_weekly: 8, keep_monthly: 12 }

const EVERY: { hours: number; label: string }[] = [
  { hours: 1, label: 'Every hour' },
  { hours: 6, label: 'Every 6 hours' },
  { hours: 12, label: 'Twice a day' },
  { hours: 24, label: 'Every day' },
  { hours: 168, label: 'Every week' },
]

function status(b: SnapshotSet): { tone: 'online' | 'offline' | 'busy' | 'error'; text: string } {
  if (b.running) return { tone: 'busy', text: 'Working…' }
  if (!b.enabled) return { tone: 'offline', text: 'Paused' }
  if (b.last_error) return { tone: b.last_result === 'Waiting for the device' ? 'offline' : 'error', text: b.last_result ?? 'Needs attention' }
  if (!b.last_run_at) return { tone: 'busy', text: 'Starting…' }
  return { tone: 'online', text: `Backed up ${formatRelative(b.last_ok_at ?? b.last_run_at)} · ${b.last_result ?? ''}` }
}

function ScheduleFields({ value, onChange }: { value: BackupSchedule; onChange: (v: BackupSchedule) => void }) {
  const num = (key: keyof BackupSchedule, label: string, min = 0) => (
    <Field label={label}>
      <Input type="number" min={min} max={1000} value={value[key]} onChange={(e) => onChange({ ...value, [key]: Number(e.target.value) })} />
    </Field>
  )
  return (
    <div className="space-y-3">
      <Field label="How often">
        <select
          value={value.every_hours}
          onChange={(e) => onChange({ ...value, every_hours: Number(e.target.value) })}
          className="h-8 w-full rounded-md border border-line bg-bg px-2 text-fg outline-none focus:border-accent"
        >
          {EVERY.map((o) => <option key={o.hours} value={o.hours}>{o.label}</option>)}
        </select>
      </Field>
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        {num('keep_last', 'Keep the latest', 1)}
        {num('keep_daily', 'Daily')}
        {num('keep_weekly', 'Weekly')}
        {num('keep_monthly', 'Monthly')}
      </div>
      <p className="text-[12px] text-subtle">
        Besides the latest snapshots, the newest one of each of the last days, weeks and months is kept. A file that did not change is
        stored only once, however many snapshots include it.
      </p>
    </div>
  )
}

/** Shows a backup's recovery key. */
function RecoveryKeyNote({ name, recoveryKey, first, onClose }: { name: string; recoveryKey: string; first: boolean; onClose: () => void }) {
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(recoveryKey)
      toast('Recovery key copied')
    } catch (e) {
      toastError(e)
    }
  }
  return (
    <Modal
      open
      onClose={onClose}
      title={`Recovery key for ${name}`}
      footer={
        <>
          <Button onClick={() => void copy()}><Copy className="size-3.5" /> Copy</Button>
          <Button variant="primary" onClick={onClose}>{first ? 'I have saved it' : 'Done'}</Button>
        </>
      }
    >
      <div className="space-y-3">
        <p>
          The backup is encrypted. If this computer is lost, this key is the only way to open the backup from another one: no one,
          including NEXUS, can recover it without the key.
        </p>
        <div className="rounded-md border border-line bg-bg p-3 text-center font-mono text-[14px] tracking-wide break-all select-all">
          {recoveryKey}
        </div>
        <p className="text-[12px] text-subtle">
          Keep it somewhere other than this computer: a password manager, or printed and put away. To open the backup elsewhere, use
          Backups → Open a backup.{first && ' You can show it again here later with your password.'}
        </p>
      </div>
    </Modal>
  )
}

function ShowRecoveryKey({ set, onClose }: { set: SnapshotSet; onClose: () => void }) {
  const security = useResource(() => api.security().catch(() => null), [])
  const twoFactor = !!security.data?.two_factor
  const [password, setPassword] = useState('')
  const [code, setCode] = useState('')
  const [shown, setShown] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  if (shown) return <RecoveryKeyNote name={set.name} recoveryKey={shown} first={false} onClose={onClose} />
  const show = async () => {
    setBusy(true)
    setError('')
    try {
      setShown((await api.snapshotRecoveryKey(set.id, password, twoFactor ? code.trim() : undefined)).recovery_key)
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal
      open
      onClose={onClose}
      title={`Recovery key for ${set.name}`}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!password || (twoFactor && !code.trim())} onClick={() => void show()}>Show the key</Button>
        </>
      }
    >
      <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); if (password) void show() }}>
        <ConfirmFields twoFactor={twoFactor} password={password} code={code} onPassword={setPassword} onCode={setCode} />
        {error && <ErrorNote>{error}</ErrorNote>}
      </form>
    </Modal>
  )
}

/** A snapshot's folders and files, with restoring. */
function SnapshotBrowser({ set, onClose }: { set: SnapshotSet; onClose: () => void }) {
  const history = useResource(() => api.snapshotHistory(set.id), [set.id])
  const [snap, setSnap] = useState<string | null>(null)
  const [path, setPath] = useState('')
  const [chosen, setChosen] = useState<string[]>([])
  const [busy, setBusy] = useState(false)
  const current = snap ?? history.data?.[0]?.id ?? null
  const entries = useResource(() => (current ? api.snapshotFiles(set.id, current, path) : Promise.resolve([])), [set.id, current, path])
  const crumbs = path ? path.split('/') : []

  const restore = async (to: 'beside' | 'original') => {
    if (!current) return
    setBusy(true)
    try {
      const r = await api.restoreSnapshot(set.id, current, { paths: chosen, to })
      toast(to === 'beside' ? `Restoring into "${r.folder}". Progress is under Transfers.` : 'Restoring over the original folder. Progress is under Transfers.')
      onClose()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  const what = chosen.length === 0 ? 'everything' : chosen.length === 1 ? `"${chosen[0].split('/').pop()}"` : `${chosen.length} items`

  return (
    <Modal
      open
      wide
      onClose={onClose}
      title={`Snapshots of ${set.name}`}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Close</Button>
          <Button disabled={!current} loading={busy} onClick={() => void restore('original')}>Restore {what} over the original</Button>
          <Button variant="primary" disabled={!current} loading={busy} onClick={() => void restore('beside')}>
            <RotateCcw className="size-3.5" /> Restore {what} beside it
          </Button>
        </>
      }
    >
      {history.error && <ErrorNote>{history.error.message}</ErrorNote>}
      {!history.data ? (
        <Spinner />
      ) : history.data.length === 0 ? (
        <p className="text-muted">No snapshots yet. The first one is made when the backup first runs.</p>
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-[220px_1fr]">
          <ul className="max-h-80 space-y-0.5 overflow-auto">
            {history.data.map((h) => (
              <li key={h.id}>
                <button
                  className={cx('w-full rounded-md px-2.5 py-1.5 text-left', h.id === current ? 'bg-accent-soft' : 'hover:bg-hover')}
                  onClick={() => { setSnap(h.id); setPath(''); setChosen([]) }}
                >
                  <div className="font-medium">{new Date(h.taken_at).toLocaleString()}</div>
                  <div className="text-[11.5px] text-subtle">
                    {h.files} files, {formatBytes(h.bytes)}{h.new_files > 0 && ` · ${h.new_files} new`}
                  </div>
                </button>
              </li>
            ))}
          </ul>
          <div className="min-w-0">
            <div className="mb-2 flex flex-wrap items-center gap-1 text-[12.5px]">
              <button className="text-accent hover:underline" onClick={() => setPath('')}>{set.name}</button>
              {crumbs.map((c, i) => (
                <span key={i} className="flex items-center gap-1">
                  <ChevronRight className="size-3 text-subtle" />
                  <button className="text-accent hover:underline" onClick={() => setPath(crumbs.slice(0, i + 1).join('/'))}>{c}</button>
                </span>
              ))}
            </div>
            {entries.error && <ErrorNote>{entries.error.message}</ErrorNote>}
            <ul className="max-h-72 divide-y divide-line overflow-auto rounded-md border border-line">
              {(entries.data ?? []).map((e) => (
                <li key={e.path} className="flex items-center gap-2 px-2.5 py-1.5">
                  <input
                    type="checkbox"
                    className="accent-[var(--nx-accent)]"
                    aria-label={`Choose ${e.name}`}
                    checked={chosen.includes(e.path)}
                    onChange={(ev) => setChosen((c) => (ev.target.checked ? [...c, e.path] : c.filter((p) => p !== e.path)))}
                  />
                  {e.is_dir ? <Folder className="size-4 text-accent" /> : <File className="size-4 text-muted" />}
                  {e.is_dir ? (
                    <button className="min-w-0 flex-1 truncate text-left hover:underline" onClick={() => setPath(e.path)}>{e.name}</button>
                  ) : (
                    <span className="min-w-0 flex-1 truncate">{e.name}</span>
                  )}
                  <span className="shrink-0 text-[12px] text-subtle tabular">
                    {e.is_dir ? `${e.files} files · ` : ''}{formatBytes(e.size)}
                  </span>
                </li>
              ))}
              {entries.data?.length === 0 && <li className="px-2.5 py-3 text-muted">Empty</li>}
            </ul>
            <p className="mt-2 text-[12px] text-subtle">
              Restoring beside the original makes a new folder next to it. Restoring over the original replaces files that differ; each
              replaced file keeps its current content as a version.
            </p>
          </div>
        </div>
      )}
    </Modal>
  )
}

function BackupCard({ set, onChanged }: { set: SnapshotSet; onChanged: () => void }) {
  const [browsing, setBrowsing] = useState(false)
  const [editing, setEditing] = useState<BackupSchedule | null>(null)
  const [confirm, setConfirm] = useState(false)
  const [showKey, setShowKey] = useState(false)
  const [busy, setBusy] = useState(false)
  const st = status(set)
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
  const every = EVERY.find((e) => e.hours === set.every_hours)?.label ?? `Every ${set.every_hours} hours`

  return (
    <li className="rounded-lg border border-line bg-panel p-4">
      <div className="grid grid-cols-1 items-center gap-2 md:grid-cols-[1fr_auto_1fr]">
        <div className="min-w-0">
          <div className="text-[11.5px] text-subtle">This computer</div>
          <div className="truncate font-medium" title={set.local_label}>{set.local_label}</div>
        </div>
        <ArrowRight className="size-4 text-accent md:mx-3" />
        <div className="min-w-0">
          <div className="flex items-center gap-1.5 text-[11.5px] text-subtle">
            {set.device_name}
            {set.encrypted ? (
              <Badge tone="accent"><Lock className="mr-1 inline size-3" />Encrypted</Badge>
            ) : (
              <Badge>Not encrypted</Badge>
            )}
          </div>
          <div className="truncate font-medium" title={set.remote_label}>{set.remote_label}</div>
        </div>
      </div>
      <div className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="flex min-w-0 items-center gap-1.5 text-[12.5px]">
          <StatusDot tone={st.tone} />
          <span className="truncate">{st.text}</span>
        </span>
        <span className="ml-auto flex flex-wrap gap-1">
          <Button size="sm" disabled={busy || set.running} onClick={() => void act(() => api.runSnapshotSet(set.id), 'Backup started')}>
            <RefreshCw className={cx('size-3.5', set.running && 'animate-spin')} /> Back up now
          </Button>
          <Button size="sm" variant="ghost" disabled={set.snapshots === 0} onClick={() => setBrowsing(true)}>
            <History className="size-3.5" /> Snapshots ({set.snapshots})
          </Button>
          <IconButton label="Check the backup" disabled={busy || set.running || set.snapshots === 0} onClick={() => void act(() => api.checkSnapshotSet(set.id), 'Checking: every stored file is read back. Progress is under Transfers.')}>
            <ShieldCheck className="size-4" />
          </IconButton>
          {set.encrypted && (
            <IconButton label="Show the recovery key" onClick={() => setShowKey(true)}>
              <KeyRound className="size-4" />
            </IconButton>
          )}
          <IconButton label="Schedule and keep rules" onClick={() => setEditing({ every_hours: set.every_hours, keep_last: set.keep_last, keep_daily: set.keep_daily, keep_weekly: set.keep_weekly, keep_monthly: set.keep_monthly })}>
            <Settings2 className="size-4" />
          </IconButton>
          <IconButton
            label={set.enabled ? 'Pause' : 'Resume'}
            disabled={busy}
            onClick={() => void act(() => api.updateSnapshotSet(set.id, { enabled: !set.enabled }), set.enabled ? 'Backup paused' : 'Backup resumed')}
          >
            {set.enabled ? <Pause className="size-4" /> : <Play className="size-4" />}
          </IconButton>
          <IconButton label="Stop backing up" className="hover:text-danger" onClick={() => setConfirm(true)}>
            <Trash2 className="size-4" />
          </IconButton>
        </span>
      </div>
      <div className="mt-2 space-y-0.5 text-[12px] text-subtle">
        <div>
          {every} · keeps {set.keep_last} latest, {set.keep_daily} daily, {set.keep_weekly} weekly, {set.keep_monthly} monthly
          {set.latest && ` · latest: ${set.latest.files} files, ${formatBytes(set.latest.bytes)}`}
        </div>
        {set.last_check_result && (
          <div className={cx(set.last_check_result.endsWith('intact') ? '' : 'text-warn')}>
            Checked {set.last_check_at ? formatRelative(set.last_check_at) : ''}: {set.last_check_result}
          </div>
        )}
      </div>
      {set.last_error && <p className="mt-2 text-[12px] break-words text-danger">{set.last_error}</p>}

      {browsing && <SnapshotBrowser set={set} onClose={() => setBrowsing(false)} />}
      {showKey && <ShowRecoveryKey set={set} onClose={() => setShowKey(false)} />}
      {editing && (
        <Modal
          open
          onClose={() => setEditing(null)}
          title={`Backup of ${set.name}`}
          footer={
            <>
              <Button variant="ghost" onClick={() => setEditing(null)}>Cancel</Button>
              <Button variant="primary" loading={busy} onClick={() => void act(() => api.updateSnapshotSet(set.id, { schedule: editing }), 'Saved').then(() => setEditing(null))}>
                Save
              </Button>
            </>
          }
        >
          <ScheduleFields value={editing} onChange={setEditing} />
        </Modal>
      )}
      <Modal
        open={confirm}
        onClose={() => setConfirm(false)}
        title="Stop backing up this folder?"
        footer={
          <>
            <Button variant="ghost" onClick={() => setConfirm(false)}>Keep backing up</Button>
            <Button variant="danger" loading={busy} onClick={() => void act(() => api.removeSnapshotSet(set.id), 'Backup stopped').then(() => setConfirm(false))}>
              Stop backing up
            </Button>
          </>
        }
      >
        <p>
          No more snapshots are made, and this computer forgets the ones it lists. What is already stored stays in {set.remote_label} on{' '}
          {set.device_name}; delete that folder there when you no longer need it.
        </p>
      </Modal>
    </li>
  )
}

function NewBackup({ devices, onClose, onCreated }: { devices: Device[]; onClose: () => void; onCreated: (set: SnapshotSet, recoveryKey: string | null) => void }) {
  const online = devices.filter((d) => d.online)
  const [device, setDevice] = useState(online[0]?.id ?? devices[0]?.id ?? '')
  const [local, setLocal] = useState({ root: '', path: '' })
  const [remote, setRemote] = useState({ root: '', path: '' })
  const [schedule, setSchedule] = useState(DEFAULT_SCHEDULE)
  const [encrypt, setEncrypt] = useState(true)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const deviceName = devices.find((d) => d.id === device)?.name ?? 'the device'

  const create = async () => {
    setBusy(true)
    setError('')
    try {
      const set = await api.createSnapshotSet({ local_root_id: local.root, local_path: local.path, device_id: device, remote_root_id: remote.root, remote_path: remote.path, schedule, encrypt })
      toast('Backup set up. The first snapshot is being made now.')
      onCreated(set, set.recovery_key)
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
      title="Back up a folder to another device"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!local.root || !remote.root || !device} onClick={() => void create()}>
            Start backing up
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <Field label="Folder on this computer">
            <PlacePicker prefix="" writable={false} value={local} onChange={setLocal} />
          </Field>
          <div className="space-y-2">
            <Field label="Device">
              <select
                value={device}
                onChange={(e) => { setDevice(e.target.value); setRemote({ root: '', path: '' }) }}
                className="h-8 w-full rounded-md border border-line bg-bg px-2 text-fg outline-none focus:border-accent"
              >
                {devices.map((d) => <option key={d.id} value={d.id} disabled={!d.online}>{d.name}{d.online ? '' : ' (offline)'}</option>)}
              </select>
            </Field>
            {device && (
              <Field label={`Where on ${deviceName}`} hint="A folder for this backup is made inside it.">
                <PlacePicker key={device} prefix={`/peers/${encodeURIComponent(device)}/proxy`} writable value={remote} onChange={setRemote} />
              </Field>
            )}
          </div>
        </div>
        <ScheduleFields value={schedule} onChange={setSchedule} />
        <label className="flex items-start gap-2">
          <input type="checkbox" className="mt-0.5 accent-[var(--nx-accent)]" checked={encrypt} onChange={(e) => setEncrypt(e.target.checked)} />
          <span>
            <span className="font-medium">Encrypt the backup</span>
            <span className="block text-[12px] text-subtle">
              {encrypt
                ? `Files and their names are stored encrypted on ${deviceName}. You get a recovery key to open the backup from another computer if this one is lost.`
                : `Anyone with access to that folder on ${deviceName} can open the stored files.`}
            </span>
          </span>
        </label>
        <p className="text-[12px] text-subtle">
          Each snapshot records the whole folder as it was, so you can bring back a file, a folder or everything from any kept snapshot,
          also after deleting or changing it here. Stored files are checked by reading them back.
        </p>
        {error && <ErrorNote>{error}</ErrorNote>}
      </div>
    </Modal>
  )
}

/** Opens a backup another computer made, to restore from it here. */
function OpenBackup({ devices, onClose, onOpened }: { devices: Device[]; onClose: () => void; onOpened: () => void }) {
  const online = devices.filter((d) => d.online)
  const [device, setDevice] = useState(online[0]?.id ?? devices[0]?.id ?? '')
  const [remote, setRemote] = useState({ root: '', path: '' })
  const [local, setLocal] = useState({ root: '', path: '' })
  const [recoveryKey, setRecoveryKey] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const deviceName = devices.find((d) => d.id === device)?.name ?? 'the device'

  const open = async () => {
    setBusy(true)
    setError('')
    try {
      const set = await api.openSnapshotSet({
        device_id: device,
        remote_root_id: remote.root,
        remote_path: remote.path,
        recovery_key: recoveryKey,
        local_root_id: local.root,
        local_path: local.path,
      })
      toast(`Opened the backup of ${set.name}: ${set.snapshots} snapshot${set.snapshots === 1 ? '' : 's'}. It is paused until you resume it.`)
      onOpened()
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
      title="Open a backup made by another computer"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!remote.root || !remote.path || !local.root || !device} onClick={() => void open()}>
            Open the backup
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <p>
          When a computer that backed up to another device is lost or set up again, open its backup here to browse and restore its
          snapshots. Resume it afterwards to keep backing up from this computer into the same place.
        </p>
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <div className="space-y-2">
            <Field label="Device holding the backup">
              <select
                value={device}
                onChange={(e) => { setDevice(e.target.value); setRemote({ root: '', path: '' }) }}
                className="h-8 w-full rounded-md border border-line bg-bg px-2 text-fg outline-none focus:border-accent"
              >
                {devices.map((d) => <option key={d.id} value={d.id} disabled={!d.online}>{d.name}{d.online ? '' : ' (offline)'}</option>)}
              </select>
            </Field>
            {device && (
              <Field label={`The backup's folder on ${deviceName}`} hint={'Named after the computer and folder, like "Desktop PC - Documents".'}>
                <PlacePicker key={device} prefix={`/peers/${encodeURIComponent(device)}/proxy`} writable={false} value={remote} onChange={setRemote} />
              </Field>
            )}
          </div>
          <Field label="Restore into, on this computer" hint="Restoring over the original goes into this folder; beside it makes a new folder next to it.">
            <PlacePicker prefix="" writable value={local} onChange={setLocal} />
          </Field>
        </div>
        <Field label="Recovery key" hint="Needed when the backup is encrypted. Capital letters, small letters and dashes all work.">
          <Input
            value={recoveryKey}
            onChange={(e) => setRecoveryKey(e.target.value)}
            placeholder="XXXX-XXXX-…"
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
          />
        </Field>
        {error && <ErrorNote>{error}</ErrorNote>}
      </div>
    </Modal>
  )
}

export function Backups() {
  const sets = useResource(() => api.snapshotSets(), [], (e) => e.type === 'snapshots.updated' || e.type === 'device.online' || e.type === 'device.offline', 'backups')
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.'), 'devices')
  const peers = (devices.data ?? []).filter((d) => !d.is_local && d.public_key && d.device_type !== 'android')
  const [adding, setAdding] = useState(false)
  const [opening, setOpening] = useState(false)
  const [newKey, setNewKey] = useState<{ name: string; key: string } | null>(null)
  const list = sets.data ?? []

  return (
    <div>
      <PageHeader
        title="Backups"
        subtitle="Snapshots of a folder on another device, to bring back any earlier state"
        actions={
          peers.length > 0 && (
            <>
              <Button onClick={() => setOpening(true)}><FolderOpen className="size-4" /> Open a backup</Button>
              <Button variant="primary" onClick={() => setAdding(true)}><History className="size-4" /> New backup</Button>
            </>
          )
        }
      />
      {sets.error && <div className="mb-3"><ErrorNote>{sets.error.message}</ErrorNote></div>}
      {!sets.data ? (
        <Spinner />
      ) : list.length === 0 ? (
        <div className="rounded-lg border border-line bg-panel">
          <EmptyState
            icon={<History className="size-8" />}
            title="Nothing is backed up yet"
            action={peers.length > 0 ? <Button variant="primary" onClick={() => setAdding(true)}>Back up a folder</Button> : undefined}
          >
            {peers.length > 0
              ? 'Pick a folder here and a place on another computer. NEXUS keeps snapshots of it there, on a schedule.'
              : 'Pair another computer first (Devices → Add a device), then back up folders to it.'}
          </EmptyState>
        </div>
      ) : (
        <ul className="space-y-3">
          {list.map((b) => <BackupCard key={b.id} set={b} onChanged={sets.reload} />)}
        </ul>
      )}
      {adding && (
        <NewBackup
          devices={peers}
          onClose={() => setAdding(false)}
          onCreated={(set, key) => {
            setAdding(false)
            sets.reload()
            if (key) setNewKey({ name: set.name, key })
          }}
        />
      )}
      {opening && (
        <OpenBackup
          devices={peers}
          onClose={() => setOpening(false)}
          onOpened={() => {
            setOpening(false)
            sets.reload()
          }}
        />
      )}
      {newKey && <RecoveryKeyNote name={newKey.name} recoveryKey={newKey.key} first onClose={() => setNewKey(null)} />}
    </div>
  )
}
