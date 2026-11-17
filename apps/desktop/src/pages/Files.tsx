import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  ArrowDown, ArrowUp, ChevronRight, Download, Eye, FolderInput, FolderPlus, HardDrive, Info, LayoutGrid,
  List, Monitor, Pencil, RotateCcw, Search, Trash2, Upload, WifiOff, X,
} from 'lucide-react'
import {
  api, ApiError, fileApi, isMobile, isTouch, peerPrefix, thisDevice, type FileApi, type FileEntry, type FoundItem, type StorageRoot, type TrashItem,
} from '../services/api'
import { subscribe } from '../services/events'
import { useResource } from '../hooks/useResource'
import { navigate, type Route } from '../hooks/useRoute'
import { enqueueUploads, onUploadComplete } from '../stores/transfers'
import { fileKind, formatBytes, formatDate, formatRelative, previewKind } from '../lib/format'
import { FileIcon } from '../components/FileIcon'
import { FilePreview, FolderPicker, downloadFile, useContentUrl } from '../components/FilePreview'
import { toast, toastError } from '../components/Toasts'
import { Badge, Button, EmptyState, ErrorNote, Field, IconButton, Input, Modal, Spinner, cx } from '../components/ui'

type Sort = 'name' | 'size' | 'modified' | 'type'
const PAGE = 300

function useListing(fapi: FileApi, rootId: string | undefined, path: string, sort: Sort, order: 'asc' | 'desc') {
  const [items, setItems] = useState<FileEntry[]>([])
  const [total, setTotal] = useState(0)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<ApiError | null>(null)
  const seq = useRef(0)

  const load = useCallback(
    async (append = false) => {
      if (!rootId) return
      const id = ++seq.current
      setLoading(true)
      try {
        const offset = append ? items.length : 0
        const res = await fapi.list({ root_id: rootId, path, sort, order, offset, limit: append ? PAGE : Math.max(PAGE, items.length) })
        if (id !== seq.current) return
        setItems((prev) => (append ? [...prev, ...res.items] : res.items))
        setTotal(res.total)
        setError(null)
      } catch (e) {
        if (id === seq.current) setError(e as ApiError)
      } finally {
        if (id === seq.current) setLoading(false)
      }
    },
    [fapi, rootId, path, sort, order, items.length],
  )

  // Reset when the location or ordering changes.
  useEffect(() => {
    setItems([])
    setTotal(0)
    setError(null)
  }, [fapi, rootId, path, sort, order])
  const loadRef = useRef(load)
  loadRef.current = load
  useEffect(() => {
    void loadRef.current(false)
  }, [fapi, rootId, path, sort, order])

  // Live refresh when something changes inside this folder.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined
    const touches = (p: string) => {
      const parent = p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : ''
      return parent.toLowerCase() === path.toLowerCase() || p.toLowerCase().startsWith(path.toLowerCase() + '/') || path === ''
    }
    const refresh = () => {
      clearTimeout(timer)
      timer = setTimeout(() => void loadRef.current(false), 350)
    }
    // Live events describe this computer's storage only; a paired device's
    // folder refreshes after changes made from here.
    const offEvents = subscribe((e) => {
      if (fapi.prefix) return
      if (e.type === 'resync') return refresh()
      if (!('root_id' in e) || e.root_id !== rootId) return
      if (e.type === 'storage.scan.completed' || e.type === 'storage.changed') return refresh()
      if (e.type === 'file.moved' && (touches(e.from) || touches(e.to))) return refresh()
      if ('path' in e && touches(e.path)) refresh()
    })
    const offUploads = onUploadComplete((t) => t.prefix === fapi.prefix && t.rootId === rootId && t.path === path && refresh())
    return () => {
      clearTimeout(timer)
      offEvents()
      offUploads()
    }
  }, [fapi, rootId, path])

  return { items, total, loading, error, reload: () => load(false), loadMore: () => load(true) }
}

function Thumb({ file, prefix }: { file: FileEntry; prefix: string }) {
  const isImage = previewKind(file) === 'image' && file.size < 25 * 1024 * 1024 && file.availability === 'online'
  const ref = useRef<HTMLDivElement>(null)
  const [visible, setVisible] = useState(false)
  useEffect(() => {
    if (!isImage || !ref.current) return
    const io = new IntersectionObserver(([e]) => e.isIntersecting && setVisible(true), { rootMargin: '200px' })
    io.observe(ref.current)
    return () => io.disconnect()
  }, [isImage])
  const url = useContentUrl(file, isImage && visible, prefix)
  return (
    <div ref={ref} className="flex aspect-[4/3] items-center justify-center overflow-hidden rounded-md bg-elevated">
      {url ? (
        <img src={url} alt="" loading="lazy" className="size-full object-cover" />
      ) : (
        <FileIcon entry={file} className="size-8" />
      )}
    </div>
  )
}

function SortHeader({ label, field, sort, order, onSort, className }: {
  label: string
  field: Sort
  sort: Sort
  order: 'asc' | 'desc'
  onSort: (s: Sort) => void
  className?: string
}) {
  const active = sort === field
  return (
    <button className={cx('flex items-center gap-1 hover:text-fg', active && 'text-fg', className)} onClick={() => onSort(field)}>
      {label}
      {active && (order === 'asc' ? <ArrowUp className="size-3" /> : <ArrowDown className="size-3" />)}
    </button>
  )
}

function Details({ file, rootName, onClose }: { file: FileEntry; rootName: string; onClose: () => void }) {
  const rows: [string, React.ReactNode][] = [
    ['Type', fileKind(file)],
    ['Size', file.is_dir ? '—' : `${formatBytes(file.size)} (${file.size.toLocaleString()} bytes)`],
    ['Location', `${rootName} / ${file.parent_path || ''}`],
    ['Modified', formatDate(file.modified_at)],
    ['Created', formatDate(file.created_at)],
    ['Indexed', formatRelative(file.indexed_at)],
    ['Availability', <Badge tone={file.availability === 'online' ? 'accent' : 'warn'}>{file.availability}</Badge>],
    ['BLAKE3', file.is_dir ? '—' : file.content_hash ? <span className="font-mono text-[11px] break-all">{file.content_hash}</span> : <span className="text-subtle">Pending (computed in background)</span>],
  ]
  return (
    <aside className="absolute inset-0 z-20 overflow-auto bg-panel md:static md:w-72 md:shrink-0 md:border-l md:border-line">
      <div className="flex h-10 items-center justify-between border-b border-line pr-2 pl-4">
        <span className="text-[12px] font-semibold tracking-wide text-muted uppercase">Properties</span>
        <IconButton label="Close" onClick={onClose}><X className="size-4" /></IconButton>
      </div>
      <div className="p-4">
        <div className="mb-4 flex items-center gap-2.5">
          <FileIcon entry={file} className="size-6" />
          <div className="min-w-0 font-medium break-words">{file.filename}</div>
        </div>
        <dl className="space-y-3">
          {rows.map(([k, v]) => (
            <div key={k}>
              <dt className="text-[11.5px] text-subtle">{k}</dt>
              <dd className="mt-0.5 break-words">{v}</dd>
            </div>
          ))}
        </dl>
      </div>
    </aside>
  )
}

function TrashView({ fapi, onBack }: { fapi: FileApi; onBack: () => void }) {
  const trash = useResource(() => fapi.trash(), [fapi])
  const [confirm, setConfirm] = useState<TrashItem | null>(null)
  const restore = async (t: TrashItem) => {
    try {
      const restored = await fapi.restoreFile(t.id)
      toast(`Restored ${restored.filename}`)
      trash.reload()
    } catch (e) {
      toastError(e)
    }
  }
  const purge = async () => {
    if (!confirm) return
    try {
      await fapi.purge(confirm.id)
      toast(`Permanently deleted ${confirm.filename}`)
      setConfirm(null)
      trash.reload()
    } catch (e) {
      toastError(e)
    }
  }
  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 pb-4">
        <Button variant="ghost" size="sm" onClick={onBack}>← Files</Button>
        <h1 className="text-[18px] font-semibold">Trash</h1>
        <span className="hidden text-muted md:inline">Deleted items stay on their drive until you delete them permanently.</span>
      </div>
      <div className="min-h-0 flex-1 overflow-auto rounded-lg border border-line bg-panel">
        {trash.loading && !trash.data ? (
          <div className="p-6"><Spinner /></div>
        ) : !trash.data?.length ? (
          <EmptyState icon={<Trash2 className="size-7" />} title="Trash is empty" />
        ) : (
          <table className="w-full text-left">
            <thead className="sticky top-0 bg-panel text-[12px] text-muted">
              <tr className="border-b border-line">
                <th className="px-4 py-2 font-medium">Name</th>
                <th className="hidden px-2 py-2 font-medium md:table-cell">Original location</th>
                <th className="hidden px-2 py-2 font-medium md:table-cell">Size</th>
                <th className="px-2 py-2 font-medium">Deleted</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {trash.data.map((t) => (
                <tr key={t.id} className="border-b border-line last:border-0 hover:bg-hover">
                  <td className="px-4 py-2">
                    <span className="flex items-center gap-2">
                      <FileIcon entry={{ is_dir: t.is_dir, mime_type: null, extension: t.filename.split('.').pop() ?? null }} />
                      {t.filename}
                    </span>
                  </td>
                  <td className="hidden px-2 py-2 text-muted md:table-cell">{t.root_name} / {t.original_path}</td>
                  <td className="hidden px-2 py-2 text-muted tabular md:table-cell">{formatBytes(t.size)}</td>
                  <td className="px-2 py-2 text-muted">{formatRelative(t.deleted_at)}</td>
                  <td className="px-4 py-2 text-right whitespace-nowrap">
                    <Button size="sm" variant="ghost" icon={<RotateCcw className="size-3.5" />} onClick={() => void restore(t)}>Restore</Button>
                    <Button size="sm" variant="ghost" className="hover:text-danger" onClick={() => setConfirm(t)}>Delete forever</Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
      <Modal
        open={!!confirm}
        onClose={() => setConfirm(null)}
        title="Delete permanently?"
        footer={
          <>
            <Button variant="ghost" onClick={() => setConfirm(null)}>Cancel</Button>
            <Button variant="danger" onClick={() => void purge()}>Delete permanently</Button>
          </>
        }
      >
        <p><span className="font-medium">{confirm?.filename}</span> will be removed from its drive. This cannot be undone.</p>
      </Modal>
    </div>
  )
}

export function Files({ route }: { route: Route }) {
  const deviceId = route.params.get('device') ?? ''
  const prefix = deviceId ? peerPrefix(deviceId) : ''
  const fapi = fileApi(prefix)
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.'))
  const peers = (devices.data ?? []).filter((d) => !d.is_local)
  const device = peers.find((d) => d.id === deviceId)
  // Remember which device the roots came from, so switching never pairs one
  // device's root id with another device.
  const roots = useResource(
    async () => ({ prefix, list: await fapi.roots() }),
    [fapi],
    (e) => !prefix && e.type === 'storage.changed',
  )
  const rootList = roots.data?.prefix === prefix ? roots.data.list : undefined
  const rootsError = roots.loading ? null : roots.error
  const rootId = route.params.get('root') ?? rootList?.[0]?.id
  const path = route.params.get('path') ?? ''
  const showTrash = route.params.get('view') === 'trash'
  const root: StorageRoot | undefined = rootList?.find((r) => r.id === rootId)

  const [sort, setSort] = useState<Sort>('name')
  const [order, setOrder] = useState<'asc' | 'desc'>('asc')
  const [view, setView] = useState<'list' | 'grid'>(() => {
    try { return (localStorage.getItem('nexus.view') ?? (isTouch() ? 'grid' : 'list')) === 'grid' ? 'grid' : 'list' } catch { return 'list' }
  })
  const listing = useListing(fapi, showTrash ? undefined : rootId, path, sort, order)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [anchor, setAnchor] = useState<string | null>(null)
  const [showDetails, setShowDetails] = useState(false)
  const [preview, setPreview] = useState<FileEntry | null>(null)
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<FoundItem[] | null>(null)
  const [dialog, setDialog] = useState<null | 'folder' | 'rename' | 'move' | 'delete'>(null)
  const [dragging, setDragging] = useState(false)
  const fileInput = useRef<HTMLInputElement>(null)

  useEffect(() => {
    try { localStorage.setItem('nexus.view', view) } catch { /* ignore */ }
  }, [view])

  useEffect(() => {
    const pre = route.params.get('select')
    setSelected(pre ? new Set([pre]) : new Set())
    setAnchor(pre)
  }, [rootId, path])

  // Entering or leaving search starts a fresh selection.
  useEffect(() => {
    setSelected(new Set())
    setAnchor(null)
  }, [results === null])

  // Debounced search across every storage root.
  useEffect(() => {
    if (!query.trim()) {
      setResults(null)
      return
    }
    const t = setTimeout(() => {
      fapi.search(query.trim()).then((r) => setResults(r.items)).catch(toastError)
    }, 220)
    return () => clearTimeout(t)
  }, [query, fapi])

  const items: FileEntry[] = results ?? listing.items
  const selectedItems = useMemo(() => items.filter((i) => selected.has(i.id)), [items, selected])
  const single = selectedItems.length === 1 ? selectedItems[0] : null
  const writable = !!root && root.online && !root.read_only

  const go = (p: { root?: string; path?: string }) =>
    navigate('files', { device: deviceId || undefined, root: p.root ?? rootId, path: p.path })
  const switchDevice = (id: string) => {
    setQuery('')
    navigate('files', { device: id || undefined })
  }
  const where = device ? `${device.name} · ` : ''

  // A phone has no storage of its own: open the first paired computer.
  useEffect(() => {
    if (!isMobile() || deviceId || peers.length === 0) return
    const first = peers.find((d) => d.online) ?? peers[0]
    navigate('files', { device: first.id })
  }, [deviceId, peers.map((d) => d.id + d.online).join()])

  const open = (f: FileEntry) => {
    if (f.is_dir) {
      setQuery('')
      go({ root: f.root_id, path: f.relative_path })
    } else setPreview(f)
  }

  const touch = isTouch()
  const click = (e: React.MouseEvent, f: FileEntry) => {
    if (touch) {
      if (selected.size > 0) toggle(f)
      else open(f)
      return
    }
    if (e.shiftKey && anchor) {
      const a = items.findIndex((i) => i.id === anchor)
      const b = items.findIndex((i) => i.id === f.id)
      const [lo, hi] = a < b ? [a, b] : [b, a]
      setSelected(new Set(items.slice(lo, hi + 1).map((i) => i.id)))
      return
    }
    if (e.ctrlKey || e.metaKey) {
      setSelected((s) => {
        const n = new Set(s)
        if (n.has(f.id)) n.delete(f.id)
        else n.add(f.id)
        return n
      })
    } else setSelected(new Set([f.id]))
    setAnchor(f.id)
  }

  const toggle = (f: FileEntry) =>
    setSelected((s) => {
      const n = new Set(s)
      if (n.has(f.id)) n.delete(f.id)
      else n.add(f.id)
      return n
    })

  const upload = (files: FileList | File[] | null) => {
    if (!files || !root) return
    const list = Array.from(files)
    if (!list.length) return
    if (!writable) {
      toast(root.read_only ? `${root.name} is read-only` : `${root.name} is offline`, 'error')
      return
    }
    enqueueUploads(root.id, where + root.name, path, list, prefix)
    toast(`Uploading ${list.length} file${list.length > 1 ? 's' : ''} to ${where}${root.name}`)
  }

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement).closest('input,textarea') || dialog || preview) return
      if (e.key === 'Delete' && selectedItems.length && writable) setDialog('delete')
      if (e.key === 'F2' && single && writable) setDialog('rename')
      if (e.key === 'Enter' && single) open(single)
      if (e.key === 'a' && (e.ctrlKey || e.metaKey)) {
        e.preventDefault()
        setSelected(new Set(items.map((i) => i.id)))
      }
      if (e.key === 'Escape') setSelected(new Set())
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  })

  if (showTrash) return <TrashView fapi={fapi} onBack={() => go({ path })} />

  const deviceSwitcher = peers.length > 0 && (
    <label className="flex items-center gap-1.5 border-r border-line pr-3">
      <Monitor className="size-4 text-subtle" />
      <span className="sr-only">Device</span>
      <select
        value={deviceId}
        onChange={(e) => switchDevice(e.target.value)}
        className="h-7 rounded-md border border-line bg-elevated px-2 text-[12.5px] text-fg outline-none focus:border-accent"
      >
        {!isMobile() && <option value="">This computer</option>}
        {peers.map((d) => (
          <option key={d.id} value={d.id}>{d.name}{d.online ? '' : ' (offline)'}</option>
        ))}
      </select>
    </label>
  )

  if (prefix && (rootsError || (rootList && rootList.length === 0))) {
    return (
      <div className="-m-4 flex h-[calc(100%+2rem)] flex-col md:-m-6 md:h-[calc(100%+3rem)]">
        <div className="flex min-h-12 shrink-0 flex-wrap items-center gap-2 border-b border-line bg-panel px-3 py-2 md:px-4 md:py-0">{deviceSwitcher}</div>
        <EmptyState
          icon={rootsError ? <WifiOff className="size-8" /> : <HardDrive className="size-8" />}
          title={rootsError ? `${device?.name ?? 'This device'} is not reachable` : `${device?.name ?? 'This device'} has no storage`}
          action={rootsError && <Button onClick={roots.reload}>Try again</Button>}
        >
          {rootsError ? rootsError.message : 'Add folders or drives on that computer to browse them here.'}
        </EmptyState>
      </div>
    )
  }

  if (!prefix && isMobile()) {
    return peers.length > 0 || devices.loading ? (
      <div className="p-6"><Spinner /></div>
    ) : (
      <EmptyState
        icon={<Monitor className="size-8" />}
        title="Connect a computer"
        action={<Button variant="primary" onClick={() => navigate('devices', { join: '1' })}>Pair with a computer</Button>}
      >
        Pair this phone with a computer that runs NEXUS to browse, download and upload its files.
      </EmptyState>
    )
  }

  if (rootList && rootList.length === 0) {
    return (
      <EmptyState
        icon={<HardDrive className="size-8" />}
        title="No storage yet"
        action={<Button variant="primary" onClick={() => navigate('storage', { add: '1' })}>Add a folder or drive</Button>}
      >
        Add a folder or drive on this {thisDevice()} to start browsing your files.
      </EmptyState>
    )
  }

  const crumbs = path ? path.split('/') : []
  const onSort = (s: Sort) => {
    if (s === sort) setOrder((o) => (o === 'asc' ? 'desc' : 'asc'))
    else {
      setSort(s)
      setOrder(s === 'modified' || s === 'size' ? 'desc' : 'asc')
    }
  }

  return (
    <div className="-m-4 flex h-[calc(100%+2rem)] flex-col md:-m-6 md:h-[calc(100%+3rem)]">
      {/* Location bar */}
      <div className="flex min-h-12 shrink-0 flex-wrap items-center gap-2 border-b border-line bg-panel px-3 py-2 md:px-4 md:py-0">
        {deviceSwitcher}
        <div className="flex min-w-0 items-center gap-1 overflow-x-auto">
          {rootList?.map((r) => (
            <button
              key={r.id}
              onClick={() => { setQuery(''); go({ root: r.id, path: '' }) }}
              className={cx(
                'flex h-7 items-center gap-1.5 rounded-md px-2.5 text-[12.5px] transition-colors',
                r.id === rootId && !results ? 'bg-accent-soft text-fg' : 'text-muted hover:bg-hover hover:text-fg',
              )}
            >
              {r.online ? <HardDrive className="size-3.5" /> : <WifiOff className="size-3.5 text-warn" />}
              {r.name}
            </button>
          ))}
        </div>
        <div className="relative w-full md:ml-auto md:w-72">
          <Search className="pointer-events-none absolute top-2 left-2.5 size-4 text-subtle" />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={device ? `Search ${device.name}` : 'Search all storage'}
            className="pl-8"
          />
          {query && (
            <button className="absolute top-2 right-2 text-subtle hover:text-fg" onClick={() => setQuery('')} aria-label="Clear search">
              <X className="size-4" />
            </button>
          )}
        </div>
      </div>

      {/* Toolbar */}
      <div className="flex min-h-11 shrink-0 flex-wrap items-center gap-x-2 gap-y-1 border-b border-line px-3 py-1 md:px-4">
        {results ? (
          <span className="text-muted">{results.length} result{results.length === 1 ? '' : 's'} for “{query}”</span>
        ) : (
          <nav className="flex min-w-0 items-center gap-1 text-[13px]">
            <button className="font-medium hover:underline" onClick={() => go({ path: '' })}>{root?.name}</button>
            {crumbs.map((c, i) => (
              <span key={i} className="flex min-w-0 items-center gap-1">
                <ChevronRight className="size-3.5 shrink-0 text-subtle" />
                <button
                  className={cx('truncate hover:underline', i === crumbs.length - 1 && 'font-medium')}
                  onClick={() => go({ path: crumbs.slice(0, i + 1).join('/') })}
                >
                  {c}
                </button>
              </span>
            ))}
            {root && !root.online && <Badge tone="warn">Offline — showing last known index</Badge>}
            {root?.read_only && <Badge>Read-only</Badge>}
          </nav>
        )}
        <div className="ml-auto flex items-center gap-1">
          {selectedItems.length > 0 ? (
            <>
              <span className="mr-1 text-[12px] text-muted">{selectedItems.length} selected</span>
              {single && !single.is_dir && (
                <>
                  <IconButton label="Preview" onClick={() => setPreview(single)}><Eye className="size-4" /></IconButton>
                  <IconButton label="Download" onClick={() => void downloadFile(single, prefix).catch(toastError)}><Download className="size-4" /></IconButton>
                </>
              )}
              {single && writable && !results && (
                <IconButton label="Rename (F2)" onClick={() => setDialog('rename')}><Pencil className="size-4" /></IconButton>
              )}
              {writable && !results && (
                <IconButton label="Move" onClick={() => setDialog('move')}><FolderInput className="size-4" /></IconButton>
              )}
              {writable && !results && (
                <IconButton label="Delete (Del)" onClick={() => setDialog('delete')} className="hover:text-danger"><Trash2 className="size-4" /></IconButton>
              )}
              <IconButton label="Properties" onClick={() => setShowDetails((v) => !v)} className={showDetails ? 'text-accent' : ''}>
                <Info className="size-4" />
              </IconButton>
              <span className="mx-1 h-5 w-px bg-line" />
            </>
          ) : null}
          <IconButton label="Trash" onClick={() => navigate('files', { device: deviceId || undefined, root: rootId, path, view: 'trash' })}><Trash2 className="size-4" /></IconButton>
          <IconButton label={view === 'list' ? 'Grid view' : 'List view'} onClick={() => setView(view === 'list' ? 'grid' : 'list')}>
            {view === 'list' ? <LayoutGrid className="size-4" /> : <List className="size-4" />}
          </IconButton>
          <Button size="sm" icon={<FolderPlus className="size-3.5" />} disabled={!writable || !!results} onClick={() => setDialog('folder')}>
            <span className="hidden sm:inline">New folder</span>
          </Button>
          <Button size="sm" variant="primary" icon={<Upload className="size-3.5" />} disabled={!writable || !!results} onClick={() => fileInput.current?.click()}>
            <span className="hidden sm:inline">Upload</span>
          </Button>
          <input ref={fileInput} type="file" multiple hidden onChange={(e) => { upload(e.target.files); e.target.value = '' }} />
        </div>
      </div>

      <div className="relative flex min-h-0 flex-1">
        <div
          className={cx('relative min-w-0 flex-1 overflow-auto', dragging && 'bg-accent-soft')}
          onDragOver={(e) => {
            if (!writable || results) return
            e.preventDefault()
            setDragging(true)
          }}
          onDragLeave={(e) => {
            if (e.currentTarget === e.target) setDragging(false)
          }}
          onDrop={(e) => {
            e.preventDefault()
            setDragging(false)
            upload(e.dataTransfer.files)
          }}
          onClick={(e) => e.target === e.currentTarget && setSelected(new Set())}
        >
          {dragging && (
            <div className="pointer-events-none absolute inset-3 z-10 flex items-center justify-center rounded-lg border-2 border-dashed border-accent text-[14px] font-medium text-accent">
              Drop to upload to {where}{root?.name}{path ? ` / ${path}` : ''}
            </div>
          )}
          {listing.error && !results ? (
            <div className="p-6">
              <ErrorNote>{listing.error.message}</ErrorNote>
              {listing.error.status === 404 && (
                <Button className="mt-3" size="sm" onClick={() => go({ path: '' })}>Go to {root?.name}</Button>
              )}
            </div>
          ) : items.length === 0 ? (
            listing.loading ? (
              <div className="p-6"><Spinner /></div>
            ) : (
              <EmptyState icon={<Upload className="size-7" />} title={results ? 'No matches' : 'This folder is empty'}>
                {!results && writable && 'Drag files here or use Upload.'}
                {!results && root?.scanning && ' Indexing is still in progress.'}
              </EmptyState>
            )
          ) : view === 'list' ? (
            <table className="w-full table-fixed text-left">
              <thead className="sticky top-0 z-[1] bg-bg text-[12px] text-muted">
                <tr className="border-b border-line">
                  <th className="w-9 pl-4">
                    <input
                      type="checkbox"
                      aria-label="Select all"
                      className="accent-[var(--nx-accent)]"
                      checked={selectedItems.length > 0 && selectedItems.length === items.length}
                      onChange={(e) => setSelected(e.target.checked ? new Set(items.map((i) => i.id)) : new Set())}
                    />
                  </th>
                  <th className="py-2 font-medium"><SortHeader label="Name" field="name" sort={sort} order={order} onSort={onSort} /></th>
                  {results && <th className="hidden w-64 font-medium md:table-cell">Location</th>}
                  <th className="w-20 pr-3 font-medium md:w-28 md:pr-0"><SortHeader label="Size" field="size" sort={sort} order={order} onSort={onSort} className="ml-auto" /></th>
                  <th className="hidden w-44 pl-6 font-medium md:table-cell"><SortHeader label="Modified" field="modified" sort={sort} order={order} onSort={onSort} /></th>
                  <th className="hidden w-24 pr-4 font-medium md:table-cell"><SortHeader label="Type" field="type" sort={sort} order={order} onSort={onSort} /></th>
                </tr>
              </thead>
              <tbody>
                {items.map((f) => {
                  const isSel = selected.has(f.id)
                  return (
                    <tr
                      key={f.id}
                      onClick={(e) => click(e, f)}
                      onDoubleClick={() => !touch && open(f)}
                      onContextMenu={(e) => { if (touch) { e.preventDefault(); toggle(f) } }}
                      className={cx('h-11 md:h-9 cursor-default border-b border-line/60 select-none', isSel ? 'bg-accent-soft' : 'hover:bg-hover')}
                    >
                      <td className="pl-4" onClick={(e) => e.stopPropagation()}>
                        <input type="checkbox" aria-label={`Select ${f.filename}`} className="accent-[var(--nx-accent)]" checked={isSel} onChange={() => toggle(f)} />
                      </td>
                      <td className="truncate pr-3">
                        <span className="flex items-center gap-2.5">
                          <FileIcon entry={f} />
                          <span className={cx('truncate', f.availability !== 'online' && 'text-muted')}>{f.filename}</span>
                          {f.availability === 'offline' && <WifiOff className="size-3.5 shrink-0 text-warn" />}
                        </span>
                      </td>
                      {results && (
                        <td className="hidden truncate pr-3 text-muted md:table-cell">
                          {(f as FoundItem).root_name}{f.parent_path ? ` / ${f.parent_path}` : ''}
                        </td>
                      )}
                      <td className="pr-3 text-right text-muted tabular md:pr-0">{f.is_dir ? '' : formatBytes(f.size)}</td>
                      <td className="hidden truncate pl-6 text-muted md:table-cell">{formatDate(f.modified_at)}</td>
                      <td className="hidden truncate pr-4 text-muted md:table-cell">{fileKind(f)}</td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          ) : (
            <div className="grid grid-cols-[repeat(auto-fill,minmax(104px,1fr))] gap-2 p-3 md:grid-cols-[repeat(auto-fill,minmax(150px,1fr))] md:gap-3 md:p-4">
              {items.map((f) => (
                <button
                  key={f.id}
                  onClick={(e) => click(e, f)}
                  onDoubleClick={() => !touch && open(f)}
                  onContextMenu={(e) => { if (touch) { e.preventDefault(); toggle(f) } }}
                  className={cx('rounded-lg border p-2 text-left select-none', selected.has(f.id) ? 'border-accent bg-accent-soft' : 'border-transparent hover:bg-hover')}
                >
                  <Thumb file={f} prefix={prefix} />
                  <div className="mt-2 truncate text-[12.5px]">{f.filename}</div>
                  <div className="truncate text-[11.5px] text-subtle">{f.is_dir ? 'Folder' : formatBytes(f.size)}</div>
                </button>
              ))}
            </div>
          )}
          {!results && listing.items.length < listing.total && (
            <div className="flex justify-center p-4">
              <Button size="sm" loading={listing.loading} onClick={() => void listing.loadMore()}>
                Load more ({listing.total - listing.items.length} remaining)
              </Button>
            </div>
          )}
        </div>
        {showDetails && single && (
          <Details file={single} rootName={(single as FoundItem).root_name ?? root?.name ?? ''} onClose={() => setShowDetails(false)} />
        )}
      </div>

      <div className="flex h-7 shrink-0 items-center gap-3 border-t border-line bg-panel px-4 text-[11.5px] text-subtle">
        {device && (
          <span className="flex items-center gap-1 text-fg">
            <Monitor className="size-3" /> {device.name}
            {device.latency_ms != null && <span className="text-subtle">· {device.latency_ms} ms</span>}
          </span>
        )}
        <span>{results ? `${results.length} results` : `${listing.total.toLocaleString()} items`}</span>
        {root?.scanning && <span className="text-warn">Indexing…</span>}
        <span className="ml-auto">{touch ? 'Tap to open · press and hold to select' : 'Double-click to open · Del to delete · F2 to rename'}</span>
      </div>

      <FilePreview file={preview} siblings={items} onNavigate={setPreview} onClose={() => setPreview(null)} prefix={prefix} />
      {root && (
        <FileDialogs
          fapi={fapi}
          kind={dialog}
          root={root}
          path={path}
          items={selectedItems}
          onClose={() => setDialog(null)}
          onDone={() => {
            setDialog(null)
            setSelected(new Set())
            void listing.reload()
          }}
        />
      )}
    </div>
  )
}

function FileDialogs({ fapi, kind, root, path, items, onClose, onDone }: {
  fapi: FileApi
  kind: null | 'folder' | 'rename' | 'move' | 'delete'
  root: StorageRoot
  path: string
  items: FileEntry[]
  onClose: () => void
  onDone: () => void
}) {
  const [name, setName] = useState('')
  const [dest, setDest] = useState(path)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  useEffect(() => {
    setError('')
    setBusy(false)
    setDest(path)
    setName(kind === 'rename' && items[0] ? items[0].filename : kind === 'folder' ? 'New folder' : '')
  }, [kind])

  const run = async (fn: () => Promise<string>) => {
    setBusy(true)
    setError('')
    try {
      toast(await fn())
      onDone()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      setBusy(false)
    }
  }

  const each = async (op: (f: FileEntry) => Promise<unknown>, verb: string) => {
    const failures: string[] = []
    for (const f of items) {
      try { await op(f) } catch (e) { failures.push(`${f.filename}: ${(e as Error).message}`) }
    }
    if (failures.length) throw new Error(failures.join('\n'))
    return `${verb} ${items.length === 1 ? items[0].filename : `${items.length} items`}`
  }

  const footer = (label: string, action: () => void, danger = false) => (
    <>
      <Button variant="ghost" onClick={onClose}>Cancel</Button>
      <Button variant={danger ? 'danger' : 'primary'} loading={busy} onClick={action}>{label}</Button>
    </>
  )

  if (kind === 'folder') {
    const submit = () => run(async () => (await fapi.createFolder(root.id, path, name.trim()), `Created ${name.trim()}`))
    return (
      <Modal open title="New folder" onClose={onClose} footer={footer('Create', submit)}>
        <form onSubmit={(e) => { e.preventDefault(); void submit() }} className="space-y-3">
          <Field label="Name"><Input value={name} onChange={(e) => setName(e.target.value)} onFocus={(e) => e.target.select()} /></Field>
          <ErrorNote>{error}</ErrorNote>
        </form>
      </Modal>
    )
  }
  if (kind === 'rename' && items[0]) {
    const submit = () => run(async () => (await fapi.updateFile(items[0].id, { name: name.trim() }), `Renamed to ${name.trim()}`))
    return (
      <Modal open title={`Rename ${items[0].filename}`} onClose={onClose} footer={footer('Rename', submit)}>
        <form onSubmit={(e) => { e.preventDefault(); void submit() }} className="space-y-3">
          <Field label="New name">
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              onFocus={(e) => {
                const dot = items[0].is_dir ? -1 : e.target.value.lastIndexOf('.')
                e.target.setSelectionRange(0, dot > 0 ? dot : e.target.value.length)
              }}
            />
          </Field>
          <ErrorNote>{error}</ErrorNote>
        </form>
      </Modal>
    )
  }
  if (kind === 'move') {
    const submit = () => run(() => each((f) => fapi.updateFile(f.id, { parent_path: dest }), `Moved`))
    return (
      <Modal open title={`Move ${items.length === 1 ? items[0].filename : `${items.length} items`}`} onClose={onClose} footer={footer(`Move to ${dest ? dest.split('/').pop() : root.name}`, submit)}>
        <div className="space-y-3">
          <FolderPicker rootId={root.id} value={dest} onChange={setDest} exclude={items.filter((i) => i.is_dir).map((i) => i.relative_path)} prefix={fapi.prefix} />
          <ErrorNote>{error}</ErrorNote>
        </div>
      </Modal>
    )
  }
  if (kind === 'delete') {
    const submit = () => run(() => each((f) => fapi.deleteFile(f.id), 'Moved to trash:'))
    return (
      <Modal open title="Move to trash?" onClose={onClose} footer={footer('Move to trash', submit, true)}>
        <div className="space-y-3">
          <p>
            {items.length === 1 ? <span className="font-medium">{items[0].filename}</span> : `${items.length} items`} will be moved to the
            trash on {root.name}. You can restore {items.length === 1 ? 'it' : 'them'} from Trash.
          </p>
          <ErrorNote>{error}</ErrorNote>
        </div>
      </Modal>
    )
  }
  return null
}
