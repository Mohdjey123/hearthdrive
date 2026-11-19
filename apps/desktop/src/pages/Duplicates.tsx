import { useMemo, useState } from 'react'
import { Copy, Trash2 } from 'lucide-react'
import { api, fileApi, isMobile, peerPrefix, thisDevice, type DuplicateGroup } from '../services/api'
import { useResource } from '../hooks/useResource'
import { navigate, type Route } from '../hooks/useRoute'
import { formatBytes, formatDate } from '../lib/format'
import { FileIcon } from '../components/FileIcon'
import { toast } from '../components/Toasts'
import { Button, EmptyState, ErrorNote, Modal, PageHeader, Spinner, cx } from '../components/ui'

const SIZES: [number, string][] = [[0, 'Any size'], [1 << 20, '1 MB or more'], [10 << 20, '10 MB or more'], [100 << 20, '100 MB or more']]

/** Files with exactly the same content, on this device or a paired one.
 *  The oldest copy is kept by default; the others can go to the trash. */
export function Duplicates({ route }: { route: Route }) {
  const deviceId = route.params.get('device') ?? (isMobile() ? null : '')
  const devices = useResource(() => api.devices().catch(() => []), [])
  const peers = (devices.data ?? []).filter((d) => !d.is_local)
  const [minSize, setMinSize] = useState(1 << 20)
  const prefix = deviceId ? peerPrefix(deviceId) : ''
  const fapi = fileApi(prefix)
  const groups = useResource(
    () => (deviceId === null ? Promise.resolve([] as DuplicateGroup[]) : fapi.duplicates(minSize || 1)),
    [prefix, minSize, deviceId],
  )
  // Files picked for the trash: by default every copy but the first (oldest).
  const [unpicked, setUnpicked] = useState<Set<string>>(new Set())
  const [picked, setPicked] = useState<Set<string>>(new Set())
  const [confirm, setConfirm] = useState(false)
  const [busy, setBusy] = useState(false)

  const chosen = useMemo(() => {
    const out = new Set<string>()
    for (const g of groups.data ?? []) {
      g.files.forEach((f, i) => {
        if (picked.has(f.id) || (i > 0 && !unpicked.has(f.id))) out.add(f.id)
      })
      // Never every copy of a group.
      if (g.files.every((f) => out.has(f.id))) out.delete(g.files[0].id)
    }
    return out
  }, [groups.data, picked, unpicked])
  const freed = (groups.data ?? []).reduce((sum, g) => sum + g.files.filter((f) => chosen.has(f.id)).length * g.size, 0)

  const toggle = (id: string, on: boolean) => {
    setPicked((s) => { const n = new Set(s); if (on) n.add(id); else n.delete(id); return n })
    setUnpicked((s) => { const n = new Set(s); if (on) n.delete(id); else n.add(id); return n })
  }

  const trash = async () => {
    setBusy(true)
    let done = 0
    let failed = 0
    for (const id of chosen) {
      try {
        await fapi.deleteFile(id)
        done++
      } catch {
        failed++
      }
    }
    setBusy(false)
    setConfirm(false)
    toast(`${done} file${done === 1 ? '' : 's'} moved to the trash${failed ? `, ${failed} could not be` : ''}`, failed ? 'error' : 'success')
    setPicked(new Set())
    setUnpicked(new Set())
    groups.reload()
  }

  const list = groups.data ?? []
  return (
    <div>
      <PageHeader
        title="Duplicate files"
        subtitle="Files with exactly the same content. Files are compared once Hearthdrive has read them, so new files show up after a while."
        actions={
          chosen.size > 0 && (
            <Button variant="primary" icon={<Trash2 className="size-3.5" />} onClick={() => setConfirm(true)}>
              Move {chosen.size} to trash ({formatBytes(freed)})
            </Button>
          )
        }
      />
      <div className="mb-4 flex flex-wrap items-center gap-2 text-[13px]">
        <select
          className="h-8 rounded-md border border-line bg-surface px-2"
          value={deviceId ?? ''}
          onChange={(e) => navigate('duplicates', { device: e.target.value || undefined })}
          aria-label="Device"
        >
          {!isMobile() && <option value="">This {thisDevice()}</option>}
          {isMobile() && deviceId === null && <option value="">Choose a device</option>}
          {peers.map((d) => <option key={d.id} value={d.id} disabled={!d.online}>{d.name}{d.online ? '' : ' (offline)'}</option>)}
        </select>
        <select className="h-8 rounded-md border border-line bg-surface px-2" value={minSize} onChange={(e) => setMinSize(Number(e.target.value))} aria-label="Size">
          {SIZES.map(([v, l]) => <option key={v} value={v}>{l}</option>)}
        </select>
        {list.length > 0 && (
          <span className="text-muted">
            {list.length} group{list.length === 1 ? '' : 's'} · {formatBytes(list.reduce((s, g) => s + g.wasted, 0))} in extra copies
          </span>
        )}
      </div>
      {groups.error ? (
        <ErrorNote>{groups.error.message}</ErrorNote>
      ) : groups.loading && !groups.data ? (
        <Spinner />
      ) : list.length === 0 ? (
        <EmptyState icon={<Copy className="size-8" />} title="No duplicates found">
          Nothing of this size is stored twice{deviceId ? ' on that device' : ''}.
        </EmptyState>
      ) : (
        <div className="space-y-3">
          {list.map((g) => (
            <div key={g.hash} className="rounded-lg border border-line bg-panel">
              <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-[12.5px] text-muted">
                <FileIcon entry={g.files[0]} />
                <span className="font-medium text-fg">{g.files[0].filename}</span>
                <span>· {g.files.length} copies of {formatBytes(g.size)}</span>
              </div>
              {g.files.map((f) => (
                <label key={f.id} className={cx('flex cursor-pointer items-center gap-3 px-4 py-1.5 text-[13px] hover:bg-hover', chosen.has(f.id) && 'text-muted line-through')}>
                  <input type="checkbox" className="accent-[var(--nx-accent)]" checked={chosen.has(f.id)} onChange={(e) => toggle(f.id, e.target.checked)} />
                  <span className="min-w-0 flex-1 truncate">{f.root_name} / {f.relative_path}</span>
                  <span className="hidden w-40 text-right text-subtle sm:block">{formatDate(f.modified_at)}</span>
                </label>
              ))}
            </div>
          ))}
        </div>
      )}
      <Modal
        open={confirm}
        onClose={() => setConfirm(false)}
        title="Move duplicates to the trash?"
        footer={
          <>
            <Button onClick={() => setConfirm(false)}>Cancel</Button>
            <Button variant="danger" disabled={busy} onClick={trash}>Move {chosen.size} to trash</Button>
          </>
        }
      >
        {chosen.size} file{chosen.size === 1 ? '' : 's'} ({formatBytes(freed)}) go to the trash, where they can be restored. At least one
        copy of each file is always kept.
      </Modal>
    </div>
  )
}
