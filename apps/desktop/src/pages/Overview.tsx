import { ArrowUpDown, Clock, HardDrive, Plus } from 'lucide-react'
import { api, type StorageRoot } from '../services/api'
import { useResource } from '../hooks/useResource'
import { navigate } from '../hooks/useRoute'
import { useTransfers } from '../stores/transfers'
import { useSession } from '../stores/session'
import { formatBytes, formatCount, formatRelative } from '../lib/format'
import { FileIcon } from '../components/FileIcon'
import { Button, EmptyState, PageHeader, Panel, ProgressBar, Spinner, StatusDot } from '../components/ui'

const ACTIONS: Record<string, string> = {
  'setup.completed': 'Completed setup',
  'auth.login': 'Signed in',
  'storage.root.added': 'Added storage',
  'storage.root.removed': 'Removed storage',
  'file.uploaded': 'Uploaded',
  'folder.created': 'Created folder',
  'file.moved': 'Moved',
  'file.trashed': 'Moved to trash',
  'file.restored': 'Restored',
  'file.purged': 'Permanently deleted',
  'device.invite.created': 'Created pairing code',
  'device.pairing.requested': 'Pairing requested',
  'device.pairing.rejected': 'Declined device',
  'device.paired': 'Paired device',
  'device.removed': 'Removed device',
  'device.unpaired.by_peer': 'Unpaired by device',
  'device.join.requested': 'Asked to join',
  'device.role.changed': 'Changed device access',
  'network.remote.enabled': 'Remote access on',
  'network.remote.disabled': 'Remote access off',
}

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
  const roots = useResource(() => api.roots(), [], (e) => e.type.startsWith('storage.'))
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

      <div className="grid grid-cols-4 gap-3">
        <Stat label="Indexed data" value={formatBytes(indexed)} sub={`${formatCount(files)} files`} />
        <Stat label="Storage locations" value={`${online.length} / ${list.length}`} sub="online" />
        <Stat label="Drive capacity" value={formatBytes(capacity)} sub={`${formatBytes(free)} free`} />
        <Stat label="Active transfers" value={String(activeTransfers.length)} sub={activeTransfers.length ? 'uploading' : 'idle'} />
      </div>

      <div className="mt-4 grid grid-cols-5 gap-4">
        <Panel
          className="col-span-3"
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
              Add existing folders or drives. NEXUS indexes them in place.
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
                        <span className="w-48 shrink-0 text-right text-[11.5px] text-subtle">{rootStatusText(r)}</span>
                      </div>
                    </button>
                  </li>
                )
              })}
            </ul>
          )}
        </Panel>

        <Panel className="col-span-2" title="Recent files" padded={false}>
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

      <div className="mt-4 grid grid-cols-5 gap-4">
        <Panel className="col-span-3" title="Activity" padded={false}>
          {activity.data?.length ? (
            <ul className="divide-y divide-line">
              {activity.data.map((a) => (
                <li key={a.id} className="flex items-center gap-3 px-4 py-2">
                  <span className="w-36 shrink-0 font-medium">{ACTIONS[a.action] ?? a.action}</span>
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
          className="col-span-2"
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
