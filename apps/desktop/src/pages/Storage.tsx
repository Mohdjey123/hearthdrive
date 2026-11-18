import { useState } from 'react'
import { FolderOpen, HardDrive, Plus, RefreshCw, Trash2, Usb } from 'lucide-react'
import { api, type StorageRoot } from '../services/api'
import { useResource } from '../hooks/useResource'
import { navigate, type Route } from '../hooks/useRoute'
import { formatBytes, formatCount, formatRelative } from '../lib/format'
import { AddStorageForm } from '../components/AddStorage'
import { toast, toastError } from '../components/Toasts'
import { Badge, Button, EmptyState, IconButton, Modal, PageHeader, ProgressBar, Spinner, StatusDot } from '../components/ui'
import { rootStatusText, rootTone } from './Overview'

export function Storage({ route }: { route: Route }) {
  const roots = useResource(() => api.roots(), [], (e) => e.type.startsWith('storage.'), 'roots:')
  const adding = route.params.get('add') === '1'
  const [removing, setRemoving] = useState<StorageRoot | null>(null)
  const [busy, setBusy] = useState(false)

  const remove = async () => {
    if (!removing) return
    setBusy(true)
    try {
      await api.removeRoot(removing.id)
      toast(`Removed ${removing.name} from NEXUS. Its files were not touched.`)
      setRemoving(null)
      roots.reload()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }

  const rescan = async (r: StorageRoot) => {
    try {
      await api.rescanRoot(r.id)
      toast(`Rescanning ${r.name}`)
      roots.reload()
    } catch (e) {
      toastError(e)
    }
  }

  const list = roots.data ?? []
  return (
    <div>
      <PageHeader
        title="Storage"
        subtitle="Folders and drives this computer contributes to your cloud"
        actions={
          <Button variant="primary" icon={<Plus className="size-3.5" />} onClick={() => navigate('storage', { add: '1' })}>
            Add storage
          </Button>
        }
      />
      <div className="rounded-lg border border-line bg-panel">
        {roots.loading && !roots.data ? (
          <div className="p-6"><Spinner /></div>
        ) : list.length === 0 ? (
          <EmptyState
            icon={<HardDrive className="size-8" />}
            title="No storage locations"
            action={<Button variant="primary" onClick={() => navigate('storage', { add: '1' })}>Add a folder or drive</Button>}
          >
            Register existing folders or whole drives. NEXUS indexes them in place and never moves your files.
          </EmptyState>
        ) : (
          <ul className="divide-y divide-line">
            {list.map((r) => {
              const used = r.disk ? r.disk.total_bytes - r.disk.available_bytes : 0
              const Icon = r.root_type === 'external' ? Usb : HardDrive
              return (
                <li key={r.id} className="grid grid-cols-1 gap-3 px-4 py-4 md:grid-cols-[1fr_auto] md:gap-4 md:px-5">
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <Icon className="size-4 text-muted" />
                      <span className="text-[14px] font-medium">{r.name}</span>
                      {r.root_type === 'external' && <Badge>External</Badge>}
                      {r.read_only && <Badge>Read-only</Badge>}
                      {!r.online && <Badge tone="warn">Offline</Badge>}
                    </div>
                    <div className="mt-0.5 truncate pl-6 font-mono text-[12px] text-subtle">{r.absolute_path}</div>
                    <div className="mt-3 grid grid-cols-2 gap-4 pl-6 text-[12px] md:grid-cols-4">
                      <div>
                        <div className="text-subtle">Indexed</div>
                        <div className="mt-0.5 tabular">{formatBytes(r.stats.bytes)}</div>
                      </div>
                      <div>
                        <div className="text-subtle">Files / folders</div>
                        <div className="mt-0.5 tabular">{formatCount(r.stats.files)} / {formatCount(r.stats.dirs)}</div>
                      </div>
                      <div>
                        <div className="text-subtle">Drive</div>
                        <div className="mt-0.5 tabular">
                          {r.disk ? `${formatBytes(r.disk.available_bytes)} free of ${formatBytes(r.disk.total_bytes)}` : '—'}
                        </div>
                      </div>
                      <div>
                        <div className="text-subtle">Status</div>
                        <div className="mt-0.5 flex items-center gap-1.5">
                          <StatusDot tone={rootTone(r)} /> {rootStatusText(r)}
                        </div>
                      </div>
                    </div>
                    <div className="mt-3 pl-6">
                      <ProgressBar value={r.disk ? used / r.disk.total_bytes : 0} tone={r.online ? 'accent' : 'muted'} />
                    </div>
                    {r.scan?.error && r.online && (
                      <div className="mt-2 pl-6 text-[12px] text-warn">
                        {r.scan.error}
                        {r.scan.completed_at && ` · last scan ${formatRelative(r.scan.completed_at)}`}
                      </div>
                    )}
                  </div>
                  <div className="flex items-start gap-1">
                    <Button size="sm" icon={<FolderOpen className="size-3.5" />} onClick={() => navigate('files', { root: r.id })}>
                      Browse
                    </Button>
                    <IconButton label="Rescan" disabled={!r.online || r.scanning} onClick={() => void rescan(r)}>
                      <RefreshCw className={r.scanning ? 'size-4 animate-spin' : 'size-4'} />
                    </IconButton>
                    <IconButton label="Remove from NEXUS" className="hover:text-danger" onClick={() => setRemoving(r)}>
                      <Trash2 className="size-4" />
                    </IconButton>
                  </div>
                </li>
              )
            })}
          </ul>
        )}
      </div>

      <Modal open={adding} onClose={() => navigate('storage')} title="Add storage">
        <AddStorageForm
          onAdded={(r) => {
            toast(`Added ${r.name}. Indexing has started.`)
            navigate('storage')
            roots.reload()
          }}
        />
      </Modal>

      <Modal
        open={!!removing}
        onClose={() => setRemoving(null)}
        title={`Remove ${removing?.name}?`}
        footer={
          <>
            <Button variant="ghost" onClick={() => setRemoving(null)}>Cancel</Button>
            <Button variant="danger" loading={busy} onClick={() => void remove()}>Remove from NEXUS</Button>
          </>
        }
      >
        <p>
          NEXUS will stop indexing <span className="font-mono text-[12px]">{removing?.absolute_path}</span> and forget its index.
        </p>
        <p className="mt-2 text-muted">Your files stay exactly where they are. Nothing on the drive is deleted.</p>
      </Modal>
    </div>
  )
}
