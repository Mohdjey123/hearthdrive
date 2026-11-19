import { ArrowUpDown, Clock, HardDrive, Monitor, Plus } from 'lucide-react'
import { api, fileApi, peerPrefix, thisDevice, type StorageRoot } from '../services/api'
import { useResource } from '../hooks/useResource'
import { navigate } from '../hooks/useRoute'
import { pageAvailable } from '../components/Sidebar'
import { HealthAlert } from './Health'
import { useTransfers } from '../stores/transfers'
import { useSession } from '../stores/session'
import { formatBytes, formatCount, formatRelative } from '../lib/format'
import { FileIcon } from '../components/FileIcon'
import { ACTIONS } from '../lib/activity'
import { Button, EmptyState, PageHeader, Panel, ProgressBar, Spinner, StatusDot } from '../components/ui'

export function rootTone(r: StorageRoot): 'online' | 'offline' | 'busy' | 'error' {
  if (!r.online) return 'offline'
  if (r.scanning) return 'busy'
  if (r.scan?.status === 'error') return 'error'
  return 'online'
}

export function rootStatusText(r: StorageRoot): string {
  if (!r.online) return 'Offline'
  if (r.scanning) return r.scan ? `Indexing · ${formatCount(r.scan.files_seen)} items` : 'Indexing'
  if (r.scan?.status === 'error') return 'Index error'
  return r.scan?.completed_at ? `Indexed ${formatRelative(r.scan.completed_at)}` : 'Online'
}

/** One device's share of the cloud: its drives counted once each. */
interface DeviceStorage {
  id: string
  name: string
  capacity: number
  free: number
  indexed: number
  files: number
  locations: number
}

function summarise(id: string, name: string, roots: StorageRoot[]): DeviceStorage {
  // Count each physical disk once even if several roots share it.
  const disks = new Map<string, { total: number; free: number }>()
  for (const r of roots) {
    if (r.online && r.disk) disks.set(`${r.disk.total_bytes}:${r.absolute_path.slice(0, 3).toLowerCase()}`, { total: r.disk.total_bytes, free: r.disk.available_bytes })
  }
  return {
    id,
    name,
    capacity: [...disks.values()].reduce((s, d) => s + d.total, 0),
    free: [...disks.values()].reduce((s, d) => s + d.free, 0),
    indexed: roots.reduce((s, r) => s + r.stats.bytes, 0),
    files: roots.reduce((s, r) => s + r.stats.files, 0),
    locations: roots.length,
  }
}

/** Storage on this computer and every paired device that answers, added up. */
function useCloudStorage(local: StorageRoot[] | undefined) {
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.'), 'devices')
  const peers = (devices.data ?? []).filter((d) => !d.is_local)
  const online = peers.filter((d) => d.online)
  const key = online.map((d) => d.id).join()
  const remote = useResource(
    async () =>
      Promise.all(
        online.map(async (d) => {
          try {
            return summarise(d.id, d.name, await fileApi(peerPrefix(d.id)).roots())
          } catch {
            return null
          }
        }),
      ),
    [key],
    (e) => e.type.startsWith('storage.'),
    `cloud:${key}`,
  )
  const here = local ? summarise('', `This ${thisDevice()}`, local) : null
  const reached = (remote.data ?? []).filter((d): d is DeviceStorage => !!d)
  const all = [...(here ? [here] : []), ...reached].filter((d) => d.locations > 0)
  const missing = peers.length - reached.length
  return { all, missing, loading: devices.loading || remote.loading }
}

function CloudStorage({ local }: { local: StorageRoot[] | undefined }) {
  const { all, missing, loading } = useCloudStorage(local)
  const capacity = all.reduce((s, d) => s + d.capacity, 0)
  const free = all.reduce((s, d) => s + d.free, 0)
  const used = capacity - free
  const indexed = all.reduce((s, d) => s + d.indexed, 0)
  const files = all.reduce((s, d) => s + d.files, 0)
  if (!all.length) return null
  return (
    <Panel className="mb-4" title="Your cloud">
      <div className="flex flex-wrap items-end justify-between gap-x-8 gap-y-2">
        <div>
          <div className="text-[12px] text-muted">Combined storage on {all.length} device{all.length === 1 ? '' : 's'}</div>
          <div className="mt-1 text-[32px] leading-none font-semibold tracking-tight tabular">{formatBytes(capacity)}</div>
        </div>
        <div className="flex gap-6 text-[12.5px]">
          <div><div className="text-muted">Used</div><div className="font-medium tabular">{formatBytes(used)}</div></div>
          <div><div className="text-muted">Free</div><div className="font-medium tabular">{formatBytes(free)}</div></div>
          <div><div className="text-muted">Indexed</div><div className="font-medium tabular">{formatBytes(indexed)}</div></div>
          <div><div className="text-muted">Files</div><div className="font-medium tabular">{formatCount(files)}</div></div>
        </div>
      </div>
      <div title={`${formatBytes(used)} used of ${formatBytes(capacity)}`}>
        <ProgressBar className="mt-4" value={capacity ? used / capacity : 0} />
      </div>
      {all.length > 1 && (
        <ul className="mt-4 space-y-2.5">
          {all.map((d) => (
            <li key={d.id} className="grid grid-cols-[minmax(0,9rem)_1fr_auto] items-center gap-3 text-[12.5px]">
              <span className="flex min-w-0 items-center gap-1.5">
                <Monitor className="size-3.5 shrink-0 text-subtle" />
                <span className="truncate">{d.name}</span>
              </span>
              <div title={`${d.name}: ${formatBytes(d.capacity - d.free)} used of ${formatBytes(d.capacity)}`}>
                <ProgressBar value={d.capacity ? (d.capacity - d.free) / d.capacity : 0} />
              </div>
              <span className="text-right text-muted tabular">
                {formatBytes(d.capacity)} <span className="text-subtle">· {formatBytes(d.free)} free</span>
              </span>
            </li>
          ))}
        </ul>
      )}
      {(missing > 0 || loading) && (
        <p className="mt-3 text-[12px] text-subtle">
          {loading ? 'Asking your devices…' : `${missing} paired device${missing === 1 ? ' is' : 's are'} offline and not counted.`}
        </p>
      )}
    </Panel>
  )
}

function Stat({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="rounded-lg border border-line bg-panel px-4 py-3.5">
      <div className="text-[12px] text-muted">{label}</div>
      <div className="mt-1 text-[22px] font-semibold tracking-tight tabular">{value}</div>
      {sub && <div className="mt-0.5 text-[12px] text-subtle">{sub}</div>}
    </div>
  )
}

export function Overview() {
  const { phase } = useSession()
  const roots = useResource(() => api.roots(), [], (e) => e.type.startsWith('storage.'), 'roots:')
  const recent = useResource(() => api.recent(8), [], (e) => e.type.startsWith('file.') || e.type === 'storage.scan.completed')
  const activity = useResource(() => api.activity(8), [], (e) => e.type.startsWith('file.') || e.type === 'storage.changed')
  const transfers = useTransfers()
  const activeTransfers = transfers.filter((t) => t.status === 'uploading' || t.status === 'queued')

  const list = roots.data ?? []
  const online = list.filter((r) => r.online)
  const indexed = list.reduce((s, r) => s + r.stats.bytes, 0)
  const files = list.reduce((s, r) => s + r.stats.files, 0)
  // Count each physical disk once even if several roots share it.
  const disks = new Map<string, { total: number; free: number }>()
  for (const r of online) {
    if (r.disk) disks.set(`${r.disk.total_bytes}:${r.absolute_path.slice(0, 3).toLowerCase()}`, { total: r.disk.total_bytes, free: r.disk.available_bytes })
  }
  const capacity = [...disks.values()].reduce((s, d) => s + d.total, 0)
  const free = [...disks.values()].reduce((s, d) => s + d.free, 0)

  return (
    <div>
      <PageHeader
        title="Overview"
        subtitle={phase.kind === 'ready' ? `${phase.nodeName} · storage node` : undefined}
        actions={
          <Button variant="primary" icon={<Plus className="size-3.5" />} onClick={() => navigate('storage', { add: '1' })}>
            Add storage
          </Button>
        }
      />

      {pageAvailable('health') && <HealthAlert onOpen={() => navigate('health')} />}

      <CloudStorage local={roots.data} />

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <Stat label="Indexed data" value={formatBytes(indexed)} sub={`${formatCount(files)} files`} />
        <Stat label="Storage locations" value={`${online.length} / ${list.length}`} sub="online" />
        <Stat label={`Drives on this ${thisDevice()}`} value={formatBytes(capacity)} sub={`${formatBytes(free)} free`} />
        <Stat label="Active transfers" value={String(activeTransfers.length)} sub={activeTransfers.length ? 'uploading' : 'idle'} />
      </div>

      <div className="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-5">
        <Panel
          className="lg:col-span-3"
          title="Storage"
          padded={false}
          actions={<Button size="sm" variant="ghost" onClick={() => navigate('storage')}>Manage</Button>}
        >
          {roots.loading && !roots.data ? (
            <div className="p-6"><Spinner /></div>
          ) : list.length === 0 ? (
            <EmptyState
              icon={<HardDrive className="size-7" />}
              title="No storage yet"
              action={<Button variant="primary" onClick={() => navigate('storage', { add: '1' })}>Add a folder or drive</Button>}
            >
              Add existing folders or drives. HearthDrive indexes them in place.
            </EmptyState>
          ) : (
            <ul className="divide-y divide-line">
              {list.map((r) => {
                const used = r.disk ? r.disk.total_bytes - r.disk.available_bytes : 0
                return (
                  <li key={r.id}>
                    <button className="w-full px-4 py-3 text-left hover:bg-hover" onClick={() => navigate('files', { root: r.id })}>
                      <div className="flex items-center gap-2">
                        <StatusDot tone={rootTone(r)} />
                        <span className="shrink-0 font-medium whitespace-nowrap">{r.name}</span>
                        <span className="min-w-0 truncate text-subtle">{r.absolute_path}</span>
                        <span className="ml-auto shrink-0 text-[12px] text-muted tabular">{formatBytes(r.stats.bytes)}</span>
                      </div>
                      <div className="mt-2 flex items-center gap-3 pl-4">
                        <ProgressBar value={r.disk ? used / r.disk.total_bytes : 0} tone={r.online ? 'accent' : 'muted'} />
                        <span className="w-28 shrink-0 text-right text-[11.5px] text-subtle sm:w-48">{rootStatusText(r)}</span>
                      </div>
                    </button>
                  </li>
                )
              })}
            </ul>
          )}
        </Panel>

        <Panel className="lg:col-span-2" title="Recent files" padded={false}>
          {recent.data?.length ? (
            <ul className="divide-y divide-line">
              {recent.data.map((f) => (
                <li key={f.id}>
                  <button
                    className="flex w-full items-center gap-2.5 px-4 py-2 text-left hover:bg-hover"
                    onClick={() => navigate('files', { root: f.root_id, path: f.parent_path, select: f.id })}
                  >
                    <FileIcon entry={f} />
                    <div className="min-w-0 flex-1">
                      <div className="truncate">{f.filename}</div>
                      <div className="truncate text-[11.5px] text-subtle">
                        {f.root_name} · {formatBytes(f.size)}
                      </div>
                    </div>
                    <span className="shrink-0 text-[11.5px] text-subtle">{formatRelative(f.modified_at)}</span>
                  </button>
                </li>
              ))}
            </ul>
          ) : (
            <EmptyState icon={<Clock className="size-6" />} title="Nothing here yet" />
          )}
        </Panel>
      </div>

      <div className="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-5">
        <Panel className="lg:col-span-3" title="Activity" padded={false}>
          {activity.data?.length ? (
            <ul className="divide-y divide-line">
              {activity.data.map((a) => (
                <li key={a.id} className="flex items-center gap-3 px-4 py-2">
                  <span className="w-28 shrink-0 font-medium sm:w-44">{ACTIONS[a.action] ?? a.action}</span>
                  <span className="min-w-0 flex-1 truncate text-muted">{a.resource}</span>
                  <span className="shrink-0 text-[11.5px] text-subtle">{formatRelative(a.timestamp)}</span>
                </li>
              ))}
            </ul>
          ) : (
            <div className="px-4 py-6 text-muted">No activity yet.</div>
          )}
        </Panel>
        <Panel
          className="lg:col-span-2"
          title="Transfers"
          actions={<Button size="sm" variant="ghost" onClick={() => navigate('transfers')}>View all</Button>}
        >
          {activeTransfers.length === 0 ? (
            <div className="flex items-center gap-2 text-muted">
              <ArrowUpDown className="size-4" /> No active transfers
            </div>
          ) : (
            <ul className="space-y-3">
              {activeTransfers.slice(0, 4).map((t) => (
                <li key={t.id}>
                  <div className="flex justify-between gap-2 text-[12.5px]">
                    <span className="truncate">{t.name}</span>
                    <span className="shrink-0 text-subtle tabular">{Math.round((t.loaded / (t.size || 1)) * 100)}%</span>
                  </div>
                  <ProgressBar className="mt-1.5" value={t.loaded / (t.size || 1)} />
                </li>
              ))}
            </ul>
          )}
        </Panel>
      </div>
    </div>
  )
}
