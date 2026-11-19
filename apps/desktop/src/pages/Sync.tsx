import { useEffect, useState } from 'react'
import { ArrowLeft, ArrowLeftRight, ArrowRight, FolderSync, Pause, Play, RefreshCw, Trash2 } from 'lucide-react'
import { api, fileApi, peerPrefix, type Device, type SyncMode, type SyncPair } from '../services/api'
import { useResource } from '../hooks/useResource'
import { formatRelative } from '../lib/format'
import { FolderPicker } from '../components/FilePreview'
import { toast, toastError } from '../components/Toasts'
import { Badge, Button, EmptyState, ErrorNote, Field, IconButton, Modal, PageHeader, Spinner, StatusDot, cx } from '../components/ui'

const MODES: { mode: SyncMode; label: string; detail: string }[] = [
  { mode: 'two_way', label: 'Both ways', detail: 'Changes on either side are copied to the other.' },
  { mode: 'upload', label: 'Only to the device', detail: 'The device’s folder becomes a copy of this one. Changes made there are overwritten.' },
  { mode: 'download', label: 'Only from the device', detail: 'This folder becomes a copy of the device’s. Changes made here are overwritten.' },
]

const ARROW = { two_way: ArrowLeftRight, upload: ArrowRight, download: ArrowLeft, copy: ArrowRight }

function status(p: SyncPair): { tone: 'online' | 'offline' | 'busy' | 'error'; text: string } {
  if (p.running) return { tone: 'busy', text: 'Syncing now…' }
  if (!p.enabled) return { tone: 'offline', text: 'Paused' }
  if (p.last_error) return { tone: p.last_result === 'Waiting for the device' ? 'offline' : 'error', text: p.last_result ?? 'Needs attention' }
  if (!p.last_run_at) return { tone: 'busy', text: 'Starting…' }
  return { tone: 'online', text: `${p.last_result ?? 'Synced'} · ${formatRelative(p.last_ok_at ?? p.last_run_at)}` }
}

function PairCard({ pair, onChanged }: { pair: SyncPair; onChanged: () => void }) {
  const [confirm, setConfirm] = useState(false)
  const [busy, setBusy] = useState(false)
  const Arrow = ARROW[pair.mode]
  const st = status(pair)
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
    <li className="rounded-lg border border-line bg-panel p-4">
      <div className="grid grid-cols-1 items-center gap-2 md:grid-cols-[1fr_auto_1fr]">
        <div className="min-w-0">
          <div className="text-[11.5px] text-subtle">This computer</div>
          <div className="truncate font-medium" title={pair.local_label}>{pair.local_label}</div>
        </div>
        <Arrow className="size-4 text-accent md:mx-3" aria-label={MODES.find((m) => m.mode === pair.mode)?.label} />
        <div className="min-w-0">
          <div className="text-[11.5px] text-subtle">{pair.device_name}</div>
          <div className="truncate font-medium" title={pair.remote_label}>{pair.remote_label}</div>
        </div>
      </div>
      <div className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="flex min-w-0 items-center gap-1.5 text-[12.5px]">
          <StatusDot tone={st.tone} />
          <span className="truncate">{st.text}</span>
        </span>
        <Badge>{MODES.find((m) => m.mode === pair.mode)?.label}</Badge>
        <span className="ml-auto flex gap-1">
          <Button size="sm" disabled={busy || pair.running} onClick={() => void act(() => api.runSync(pair.id), 'Sync started')}>
            <RefreshCw className={cx('size-3.5', pair.running && 'animate-spin')} /> Sync now
          </Button>
          <IconButton
            label={pair.enabled ? 'Pause' : 'Resume'}
            disabled={busy}
            onClick={() => void act(() => api.updateSync(pair.id, { enabled: !pair.enabled }), pair.enabled ? 'Sync paused' : 'Sync resumed')}
          >
            {pair.enabled ? <Pause className="size-4" /> : <Play className="size-4" />}
          </IconButton>
          <IconButton label="Stop syncing" className="hover:text-danger" onClick={() => setConfirm(true)}>
            <Trash2 className="size-4" />
          </IconButton>
        </span>
      </div>
      {pair.last_error && <p className="mt-2 text-[12px] break-words text-danger">{pair.last_error}</p>}
      <Modal
        open={confirm}
        onClose={() => setConfirm(false)}
        title="Stop syncing these folders?"
        footer={
          <>
            <Button variant="ghost" onClick={() => setConfirm(false)}>Keep syncing</Button>
            <Button variant="danger" loading={busy} onClick={() => void act(() => api.removeSync(pair.id), 'Sync removed').then(() => setConfirm(false))}>
              Stop syncing
            </Button>
          </>
        }
      >
        <p>Both folders stay as they are now, on this computer and on {pair.device_name}. They just stop being kept in step.</p>
      </Modal>
    </li>
  )
}

/** Picks a storage location and a folder in it, on this computer or a device. */
export function PlacePicker({ prefix, writable, value, onChange }: {
  prefix: string
  writable: boolean
  value: { root: string; path: string }
  onChange: (v: { root: string; path: string }) => void
}) {
  const roots = useResource(async () => ({ prefix, list: (await fileApi(prefix).roots()).filter((r) => r.online && (!writable || !r.read_only)) }), [prefix, writable])
  const list = roots.data?.prefix === prefix ? roots.data.list : undefined
  const root = list?.find((r) => r.id === value.root) ?? list?.[0]
  useEffect(() => {
    if (root && root.id !== value.root) onChange({ root: root.id, path: '' })
  }, [root?.id])
  if (roots.error) return <ErrorNote>{roots.error.message}</ErrorNote>
  if (!list) return <Spinner />
  if (!root) return <p className="text-muted">No storage you can {writable ? 'write to' : 'read'} here.</p>
  return (
    <div className="space-y-2">
      <select
        value={root.id}
        onChange={(e) => onChange({ root: e.target.value, path: '' })}
        aria-label="Storage"
        className="h-8 w-full rounded-md border border-line bg-bg px-2 text-fg outline-none focus:border-accent"
      >
        {list.map((r) => <option key={r.id} value={r.id}>{r.name}</option>)}
      </select>
      <FolderPicker rootId={root.id} value={value.path} onChange={(path) => onChange({ root: root.id, path })} prefix={prefix} />
    </div>
  )
}

function NewSync({ devices, onClose, onCreated }: { devices: Device[]; onClose: () => void; onCreated: () => void }) {
  const online = devices.filter((d) => d.online)
  const [device, setDevice] = useState(online[0]?.id ?? devices[0]?.id ?? '')
  const [mode, setMode] = useState<SyncMode>('two_way')
  const [local, setLocal] = useState({ root: '', path: '' })
  const [remote, setRemote] = useState({ root: '', path: '' })
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const deviceName = devices.find((d) => d.id === device)?.name ?? 'the device'

  const create = async () => {
    setBusy(true)
    setError('')
    try {
      await api.createSync({ local_root_id: local.root, local_path: local.path, device_id: device, remote_root_id: remote.root, remote_path: remote.path, mode })
      toast('Sync set up. The first run starts now.')
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
      title="Sync a folder with another device"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!local.root || !remote.root || !device} onClick={() => void create()}>
            Start syncing
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <Field label="Folder on this computer">
            <PlacePicker prefix="" writable={mode !== 'upload'} value={local} onChange={setLocal} />
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
              <Field label={`Folder on ${deviceName}`}>
                <PlacePicker key={device} prefix={peerPrefix(device)} writable={mode !== 'download'} value={remote} onChange={setRemote} />
              </Field>
            )}
          </div>
        </div>
        <fieldset>
          <legend className="mb-1.5 text-[12px] text-muted">Direction</legend>
          <div className="grid grid-cols-1 gap-2 md:grid-cols-3">
            {MODES.map((m) => {
              const Arrow = ARROW[m.mode]
              return (
                <label
                  key={m.mode}
                  className={cx(
                    'flex cursor-pointer gap-2 rounded-md border p-3',
                    mode === m.mode ? 'border-accent bg-accent-soft' : 'border-line hover:bg-hover',
                  )}
                >
                  <input type="radio" name="mode" className="mt-0.5 accent-[var(--nx-accent)]" checked={mode === m.mode} onChange={() => setMode(m.mode)} />
                  <span>
                    <span className="flex items-center gap-1.5 font-medium"><Arrow className="size-3.5" /> {m.label}</span>
                    <span className="mt-0.5 block text-[12px] text-subtle">{m.detail}</span>
                  </span>
                </label>
              )
            })}
          </div>
        </fieldset>
        <p className="text-[12px] text-subtle">
          Nothing is deleted outright: a file removed on one side goes to the trash on the other, and a replaced file keeps its
          earlier version. If a file changes on both sides, the newer one keeps its name and the other is kept beside it.
          Sync runs within a minute of a change here and every five minutes otherwise, while both devices are on.
        </p>
        {error && <ErrorNote>{error}</ErrorNote>}
      </div>
    </Modal>
  )
}

export function Sync() {
  const pairs = useResource(() => api.syncPairs(), [], (e) => e.type === 'sync.updated' || e.type === 'device.online' || e.type === 'device.offline')
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.'))
  const peers = (devices.data ?? []).filter((d) => !d.is_local && d.public_key)
  const [adding, setAdding] = useState(false)
  // Extra copies have their own page.
  const list = (pairs.data ?? []).filter((p) => p.mode !== 'copy')

  return (
    <div>
      <PageHeader
        title="Sync"
        subtitle="Keep a folder the same on this computer and another device"
        actions={peers.length > 0 && <Button variant="primary" onClick={() => setAdding(true)}><FolderSync className="size-4" /> New sync</Button>}
      />
      {pairs.error && <div className="mb-3"><ErrorNote>{pairs.error.message}</ErrorNote></div>}
      {!pairs.data ? (
        <Spinner />
      ) : list.length === 0 ? (
        <div className="rounded-lg border border-line bg-panel">
          <EmptyState
            icon={<FolderSync className="size-8" />}
            title="No folders are synced yet"
            action={peers.length > 0 ? <Button variant="primary" onClick={() => setAdding(true)}>Sync a folder</Button> : undefined}
          >
            {peers.length > 0
              ? 'Pick a folder here and a folder on another computer. HearthDrive keeps them the same, both ways or one way.'
              : 'Pair another computer first (Devices → Add a device), then sync folders with it.'}
          </EmptyState>
        </div>
      ) : (
        <ul className="space-y-3">
          {list.map((p) => <PairCard key={p.id} pair={p} onChanged={pairs.reload} />)}
        </ul>
      )}
      {adding && (
        <NewSync
          devices={peers}
          onClose={() => setAdding(false)}
          onCreated={() => {
            setAdding(false)
            pairs.reload()
          }}
        />
      )}
    </div>
  )
}
