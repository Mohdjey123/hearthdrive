import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  ArrowDown, ArrowUp, ChevronRight, CopyPlus, Download, ExternalLink, Eye, FileArchive, FolderInput, FolderPlus, Globe, HardDrive, History, Info,
  Inbox, LayoutGrid, Link2, List, Lock, Monitor, Pencil, RotateCcw, Search, Share2, SlidersHorizontal, Trash2, Upload, Users, WifiOff, X,
} from 'lucide-react'
import { ShareDialog } from '../components/ShareDialog'
import { AccessBadge, Comments, ShareFolderDialog } from '../components/FolderSharing'
import { EncryptDialog, PluginsButton } from '../components/FolderTools'
import { AskForFilesDialog } from '../components/UploadLinks'
import {
  api, ApiError, fileApi, isMobile, libraryApi, isTouch, peerPrefix, thisDevice, type Device, type FileApi, type FileEntry, type FoundItem,
  type FileKindFilter, type RootSummary, type SearchFilters, type SharedWithMe, type TrashItem,
} from '../services/api'
import { useSession } from '../stores/session'
import { subscribe } from '../services/events'
import { useResource } from '../hooks/useResource'
import { cached, remember } from '../lib/cache'
import { navigate, type Route } from '../hooks/useRoute'
import { enqueueUploads, onUploadComplete } from '../stores/transfers'
import { fileKind, formatBytes, formatDate, formatRelative, previewKind } from '../lib/format'
import { FileIcon } from '../components/FileIcon'
import { FilePreview, FolderPicker, canHandOff, downloadFile, openWithApp, saveUrl, shareWithApps, useContentUrl } from '../components/FilePreview'
import { toast, toastError } from '../components/Toasts'
import { Badge, Button, EmptyState, ErrorNote, Field, IconButton, Input, Modal, Spinner, cx } from '../components/ui'

type Sort = 'name' | 'size' | 'modified' | 'type'

const KIND_OPTIONS: [FileKindFilter | '', string][] = [
  ['', 'Any type'], ['image', 'Photos and images'], ['video', 'Videos'], ['audio', 'Music and audio'], ['document', 'Documents'],
  ['archive', 'Archives'], ['code', 'Code'], ['folder', 'Folders'], ['other', 'Other files'],
]
/** Choose a tag to search by (tags people gave, and ones HearthDrive gave documents). */
function TagFilter({ prefix, value, onChange }: { prefix: string; value: string; onChange: (t: string) => void }) {
  const [tags, setTags] = useState<{ tag: string; files: number }[]>([])
  useEffect(() => {
    libraryApi(prefix).tags().then(setTags).catch(() => setTags([]))
  }, [prefix])
  if (!tags.length && !value) return null
  return (
    <select className="h-7 rounded-md border border-line bg-surface px-2" value={value} onChange={(e) => onChange(e.target.value)} aria-label="Tag">
      <option value="">Any tag</option>
      {tags.map((t) => <option key={t.tag} value={t.tag}>{t.tag} ({t.files})</option>)}
    </select>
  )
}

const WHEN_OPTIONS: [string, string, number][] = [
  ['', 'Any time', 0], ['day', 'Last 24 hours', 1], ['week', 'Last 7 days', 7], ['month', 'Last 30 days', 30], ['year', 'Last year', 365],
]
const ARCHIVE_RE = /\.(zip|cbz|7z|cb7|tar|tgz|tar\.gz)$/i
export const isArchive = (name: string) => ARCHIVE_RE.test(name)

/** A search result, tagged with the device it lives on when searching all
 *  devices (`''` = this device). Untagged results belong to the device being browsed. */
type Found = FoundItem & { device_id?: string; device_name?: string }
const PAGE = 300

const listingKey = (fapi: FileApi, rootId: string, path: string, sort: Sort, order: string) =>
  `list:${fapi.prefix}:${rootId}:${path.toLowerCase()}:${sort}:${order}`

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
        if (!append) remember(listingKey(fapi, rootId, path, sort, order), { items: res.items.slice(0, PAGE), total: res.total })
      } catch (e) {
        if (id === seq.current) setError(e as ApiError)
      } finally {
        if (id === seq.current) setLoading(false)
      }
    },
    [fapi, rootId, path, sort, order, items.length],
  )

  // Reset when the location or ordering changes: to what this folder
  // showed last time, if it was seen before, while it loads again.
  useEffect(() => {
    const known = rootId ? cached<{ items: FileEntry[]; total: number }>(listingKey(fapi, rootId, path, sort, order)) : undefined
    setItems(known?.items ?? [])
    setTotal(known?.total ?? 0)
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

/** Earlier contents of a file, kept whenever HearthDrive replaced it. */
function Versions({ file, fapi, writable, onRestored }: { file: FileEntry; fapi: FileApi; writable: boolean; onRestored: () => void }) {
  const versions = useResource(() => fapi.versions(file.id), [file.id, fapi], (e) => e.type === 'file.modified')
  const [busy, setBusy] = useState<string | null>(null)
  const restore = async (id: string) => {
    setBusy(id)
    try {
      await fapi.restoreVersion(file.id, id)
      toast('Earlier version restored. The one it replaced is kept in the list.')
      versions.reload()
      onRestored()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(null)
    }
  }
  const list = versions.data ?? []
  return (
    <section className="mt-5 border-t border-line pt-4">
      <h4 className="flex items-center gap-1.5 text-[11.5px] font-semibold tracking-wide text-muted uppercase">
        <History className="size-3.5" /> Earlier versions
      </h4>
      {versions.error ? (
        <p className="mt-2 text-[12px] text-subtle">{versions.error.message}</p>
      ) : !versions.data ? (
        <Spinner className="mt-2" />
      ) : list.length === 0 ? (
        <p className="mt-2 text-[12px] text-subtle">None yet. When sync or a copy replaces this file, the old content is kept here.</p>
      ) : (
        <ul className="mt-2 space-y-2">
          {list.map((v) => (
            <li key={v.id} className="rounded-md border border-line p-2.5">
              <div className="text-[12.5px]">{formatDate(v.modified_at ?? v.created_at)}</div>
              <div className="text-[11.5px] text-subtle">
                {formatBytes(v.size)} · replaced {formatRelative(v.created_at)}{v.created_by ? ` by ${v.created_by}` : ''}
              </div>
              <div className="mt-2 flex gap-1.5">
                <Button size="sm" onClick={() => void fapi.versionUrl(file.id, v.id).then((u) => saveUrl(u, file.filename)).catch(toastError)}>
                  <Download className="size-3.5" /> Download
                </Button>
                {writable && (
                  <Button size="sm" loading={busy === v.id} disabled={busy !== null} onClick={() => void restore(v.id)}>
                    <RotateCcw className="size-3.5" /> Restore
                  </Button>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

function Details({ file, rootName, fapi, writable, onChanged, onClose }: {
  file: FileEntry
  rootName: string
  fapi: FileApi
  writable: boolean
  onChanged: () => void
  onClose: () => void
}) {
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
        {!file.is_dir && <Versions file={file} fapi={fapi} writable={writable} onRestored={onChanged} />}
        <Comments file={file} fapi={fapi} />
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

/** The innermost folder shared with the caller that holds `path`. */
function shareAt(list: SharedWithMe[], rootId: string, path: string) {
  const p = path.toLowerCase()
  return list
    .filter((s) => s.root.id === rootId && (s.path === '' || p === s.path.toLowerCase() || p.startsWith(s.path.toLowerCase() + '/')))
    .sort((a, b) => b.path.length - a.path.length)[0]
}

export function Files({ route }: { route: Route }) {
  const deviceId = route.params.get('device') ?? ''
  const prefix = deviceId ? peerPrefix(deviceId) : ''
  const fapi = fileApi(prefix)
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.'), 'devices')
  const peers = (devices.data ?? []).filter((d) => !d.is_local)
  const device = peers.find((d) => d.id === deviceId)
  // Remember which device the roots came from, so switching never pairs one
  // device's root id with another device.
  const roots = useResource(
    async () => ({ prefix, list: await fapi.roots() }),
    [fapi],
    (e) => !prefix && e.type === 'storage.changed',
    `files-roots:${prefix}`,
  )
  const rootList = roots.data?.prefix === prefix ? roots.data.list : undefined
  const rootsError = roots.loading ? null : roots.error
  const { phase } = useSession()
  const manager = phase.kind === 'ready' && (phase.me.role === 'owner' || phase.me.role === 'administrator')
  // Folders shared with the person signed in (or, on a paired device, with
  // this device), in storage they cannot see as a whole.
  const mine = useResource(() => fapi.sharedWithMe().catch(() => []), [fapi], undefined, `shared-mine:${prefix}`)
  const sharedList = (mine.data ?? []).filter((s) => !s.sees_all)
  const firstShare = rootList && rootList.length === 0 ? sharedList[0] : undefined
  const rootId = route.params.get('root') ?? rootList?.[0]?.id ?? firstShare?.root.id
  const path = route.params.get('path') ?? firstShare?.path ?? ''
  const showTrash = route.params.get('view') === 'trash'
  const fullRoot = rootList?.find((r) => r.id === rootId)
  const share = !fullRoot && rootId ? shareAt(sharedList, rootId, path) : undefined
  const root: RootSummary | undefined = fullRoot ?? share?.root
  // A drop folder takes files without showing what is in it.
  const dropOnly = share?.access === 'add'

  const [sort, setSort] = useState<Sort>('name')
  const [order, setOrder] = useState<'asc' | 'desc'>('asc')
  const [view, setView] = useState<'list' | 'grid'>(() => {
    try { return (localStorage.getItem('nexus.view') ?? (isTouch() ? 'grid' : 'list')) === 'grid' ? 'grid' : 'list' } catch { return 'list' }
  })
  const listing = useListing(fapi, showTrash || dropOnly ? undefined : rootId, path, sort, order)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [anchor, setAnchor] = useState<string | null>(null)
  const [showDetails, setShowDetails] = useState(false)
  // With the Properties panel open the list is narrower: drop the less
  // important columns earlier so names stay readable.
  const narrow = showDetails
  const [preview, setPreview] = useState<FileEntry | null>(null)
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<Found[] | null>(null)
  const [searching, setSearching] = useState(false)
  const [everywhere, setEverywhere] = useState(false)
  const [filtering, setFiltering] = useState(false)
  const [kind, setKind] = useState<FileKindFilter | ''>('')
  const [when, setWhen] = useState('')
  const [availableOnly, setAvailableOnly] = useState(false)
  const [tag, setTag] = useState('')
  const [fetching, setFetching] = useState(false)
  const filtered = !!kind || !!when || availableOnly || !!tag
  const [copying, setCopying] = useState(false)
  const [sharing, setSharing] = useState<FileEntry | null>(null)
  const [sharingFolder, setSharingFolder] = useState<FileEntry | null>(null)
  const [encrypting, setEncrypting] = useState<FileEntry | null>(null)
  const [asking, setAsking] = useState<FileEntry | null>(null)
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

  // Debounced search: the device being browsed, or every device at once.
  const peerKey = peers.map((d) => d.id + d.online).join()
  useEffect(() => {
    const text = query.trim()
    if (!text && !filtered) {
      setResults(null)
      setSearching(false)
      return
    }
    const days = WHEN_OPTIONS.find((w) => w[0] === when)?.[2] ?? 0
    const filters: SearchFilters = {
      kind: kind || undefined,
      modified_after: days ? Date.now() - days * 86400000 : undefined,
      available: availableOnly || undefined,
      tag: tag || undefined,
    }
    let live = true
    setSearching(true)
    const t = setTimeout(() => {
      const run = async (): Promise<Found[]> => {
        if (!everywhere) return (await fapi.search(text, filters)).items
        const sources = [
          ...(isMobile() ? [] : [{ id: '', name: `This ${thisDevice()}` }]),
          ...peers.filter((d) => d.online).map((d) => ({ id: d.id, name: d.name })),
        ]
        const found = await Promise.allSettled(
          sources.map(async (src) =>
            (await fileApi(src.id ? peerPrefix(src.id) : '').search(text, filters)).items.map((i) => ({ ...i, device_id: src.id, device_name: src.name })),
          ),
        )
        const failed = found.filter((r) => r.status === 'rejected').length
        if (failed) toast(`${failed} device${failed > 1 ? 's' : ''} could not be searched`, 'error')
        return found.flatMap((r) => (r.status === 'fulfilled' ? r.value : []))
      }
      run()
        .then((items) => live && setResults(items))
        .catch((e) => live && toastError(e))
        .finally(() => live && setSearching(false))
    }, 220)
    return () => {
      live = false
      clearTimeout(t)
    }
  }, [query, fapi, everywhere, peerKey, kind, when, availableOnly, tag])

  /** Which device's API serves this entry: its own for tagged results. */
  const deviceOf = (f: FileEntry) => (f as Found).device_id ?? deviceId
  const prefixOf = (f: FileEntry) => {
    const id = deviceOf(f)
    return id ? peerPrefix(id) : ''
  }

  const items: FileEntry[] = results ?? listing.items
  const selectedItems = useMemo(() => items.filter((i) => selected.has(i.id)), [items, selected])
  const single = selectedItems.length === 1 ? selectedItems[0] : null
  const writable = !!root && root.online && !root.read_only && (!share || share.access === 'edit')
  // Adding files and folders: a drop folder allows that much.
  const canAdd = !!root && root.online && !root.read_only && (!share || share.access !== 'view')

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
      navigate('files', { device: deviceOf(f) || undefined, root: f.root_id, path: f.relative_path })
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
    if (!canAdd) {
      toast(root.read_only ? `${root.name} is read-only` : !root.online ? `${root.name} is offline` : 'This folder is shared with you to view only', 'error')
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

  if (prefix && (rootsError || (rootList && rootList.length === 0 && mine.data && sharedList.length === 0))) {
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
        Pair this phone with a computer that runs HearthDrive to browse, download and upload its files.
      </EmptyState>
    )
  }

  if (rootList && rootList.length === 0 && mine.data && sharedList.length === 0) {
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

  // Inside a shared folder, the path starts at that folder.
  const base = share?.path ?? ''
  const rel = share ? path.slice(base.length).replace(/^\//, '') : path
  const crumbs = rel ? rel.split('/') : []
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
          {sharedList.map((s) => (
            <button
              key={s.id}
              title={`Shared by ${s.shared_by}`}
              onClick={() => { setQuery(''); go({ root: s.root.id, path: s.path }) }}
              className={cx(
                'flex h-7 items-center gap-1.5 rounded-md px-2.5 text-[12.5px] transition-colors',
                share?.id === s.id && !results ? 'bg-accent-soft text-fg' : 'text-muted hover:bg-hover hover:text-fg',
              )}
            >
              {s.root.online ? <Users className="size-3.5" /> : <WifiOff className="size-3.5 text-warn" />}
              {s.name}
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
            <button className="absolute top-2 right-8 text-subtle hover:text-fg" onClick={() => setQuery('')} aria-label="Clear search">
              <X className="size-4" />
            </button>
          )}
          <button
            className={cx('absolute top-2 right-2', filtering || filtered ? 'text-accent' : 'text-subtle hover:text-fg')}
            onClick={() => setFiltering((v) => !v)}
            aria-label="Filters"
            title="Filter by type, date and availability"
          >
            <SlidersHorizontal className="size-4" />
          </button>
        </div>
      </div>

      {(filtering || filtered) && (
        <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-line px-3 py-1.5 text-[12.5px] md:px-4">
          <select className="h-7 rounded-md border border-line bg-surface px-2" value={kind} onChange={(e) => setKind(e.target.value as FileKindFilter | '')} aria-label="Type">
            {KIND_OPTIONS.map(([v, label]) => <option key={v} value={v}>{label}</option>)}
          </select>
          <select className="h-7 rounded-md border border-line bg-surface px-2" value={when} onChange={(e) => setWhen(e.target.value)} aria-label="Modified">
            {WHEN_OPTIONS.map(([v, label]) => <option key={v} value={v}>{v ? `Changed: ${label.toLowerCase()}` : 'Changed any time'}</option>)}
          </select>
          <TagFilter prefix={fapi.prefix} value={tag} onChange={setTag} />
          <label className="flex items-center gap-1.5">
            <input type="checkbox" className="accent-[var(--nx-accent)]" checked={availableOnly} onChange={(e) => setAvailableOnly(e.target.checked)} />
            Available now
          </label>
          {filtered && (
            <button className="text-muted hover:text-fg hover:underline" onClick={() => { setKind(''); setWhen(''); setAvailableOnly(false); setTag('') }}>
              Clear filters
            </button>
          )}
        </div>
      )}

      {/* Toolbar */}
      <div className="flex min-h-11 shrink-0 flex-wrap items-center gap-x-2 gap-y-1 border-b border-line px-3 py-1 md:px-4">
        {results ? (
          <span className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-muted">
            <span className="truncate">
              {searching
                ? 'Searching…'
                : `${results.length} result${results.length === 1 ? '' : 's'}${query.trim() ? ` for “${query}”` : ''}`}
            </span>
            {peers.length > 0 && (
              <label className="flex shrink-0 items-center gap-1.5 text-[12.5px] text-fg">
                <input type="checkbox" className="accent-[var(--nx-accent)]" checked={everywhere} onChange={(e) => setEverywhere(e.target.checked)} />
                All devices
              </label>
            )}
          </span>
        ) : (
          <nav className="flex min-w-0 items-center gap-1 text-[13px]">
            <button className="font-medium hover:underline" onClick={() => go({ path: base })}>{share ? share.name : root?.name}</button>
            {crumbs.map((c, i) => (
              <span key={i} className="flex min-w-0 items-center gap-1">
                <ChevronRight className="size-3.5 shrink-0 text-subtle" />
                <button
                  className={cx('truncate hover:underline', i === crumbs.length - 1 && 'font-medium')}
                  onClick={() => go({ path: [base, ...crumbs.slice(0, i + 1)].filter(Boolean).join('/') })}
                >
                  {c}
                </button>
              </span>
            ))}
            {root && !root.online && <Badge tone="warn">Offline — showing last known index</Badge>}
            {root?.read_only && <Badge>Read-only</Badge>}
            {share && <AccessBadge access={share.access} />}
          </nav>
        )}
        <div className="ml-auto flex items-center gap-1">
          {selectedItems.length > 0 ? (
            <>
              <span className="mr-1 text-[12px] text-muted">{selectedItems.length} selected</span>
              {single && !single.is_dir && (
                <>
                  <IconButton label="Preview" onClick={() => setPreview(single)}><Eye className="size-4" /></IconButton>
                  <IconButton label="Download" onClick={() => void downloadFile(single, prefixOf(single)).catch(toastError)}><Download className="size-4" /></IconButton>
                  {canHandOff() && (
                    <IconButton label="Open in another app" onClick={() => void openWithApp(single, prefixOf(single)).catch(toastError)}><ExternalLink className="size-4" /></IconButton>
                  )}
                </>
              )}
              {canHandOff() && selectedItems.every((f) => !f.is_dir) && selectedItems.length <= 20 && (
                <IconButton
                  label="Share"
                  onClick={() => void shareWithApps(selectedItems.map((f) => ({ file: f, prefix: prefixOf(f) }))).catch(toastError)}
                >
                  <Share2 className="size-4" />
                </IconButton>
              )}
              {single && single.availability === 'online' && (
                <IconButton label="Share a link" onClick={() => setSharing(single)}><Link2 className="size-4" /></IconButton>
              )}
              {single && !single.is_dir && isArchive(single.filename) && canAdd && !results && (
                <IconButton
                  label="Extract here"
                  onClick={() =>
                    void fileApi(prefixOf(single)).extract(single.id)
                      .then((r) => toast(`Extracting into ${r.path.split('/').pop()} — follow it under Transfers`))
                      .catch(toastError)
                  }
                >
                  <FileArchive className="size-4" />
                </IconButton>
              )}
              {single?.is_dir && manager && !prefix && !results && (
                <IconButton label="Share with people" onClick={() => setSharingFolder(single)}><Users className="size-4" /></IconButton>
              )}
              {single?.is_dir && writable && !prefix && !results && (
                <IconButton label="Encrypt or decrypt" onClick={() => setEncrypting(single)}><Lock className="size-4" /></IconButton>
              )}
              {single?.is_dir && writable && !prefix && !results && !isMobile() && (
                <IconButton label="Ask for files" onClick={() => setAsking(single)}><Inbox className="size-4" /></IconButton>
              )}
              {single && manager && !prefix && single.availability === 'online' && <PluginsButton entry={single} />}
              {peers.length > 0 && (
                <IconButton label="Copy to another device" onClick={() => setCopying(true)}><CopyPlus className="size-4" /></IconButton>
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
          <IconButton label="Download from a link" disabled={!canAdd || !!results} onClick={() => setFetching(true)}>
            <Globe className="size-4" />
          </IconButton>
          <Button size="sm" icon={<FolderPlus className="size-3.5" />} disabled={!canAdd || !!results} onClick={() => setDialog('folder')}>
            <span className="hidden sm:inline">New folder</span>
          </Button>
          <Button size="sm" variant="primary" icon={<Upload className="size-3.5" />} disabled={!canAdd || !!results} onClick={() => fileInput.current?.click()}>
            <span className="hidden sm:inline">Upload</span>
          </Button>
          <input ref={fileInput} type="file" multiple hidden onChange={(e) => { upload(e.target.files); e.target.value = '' }} />
        </div>
      </div>

      <div className="relative flex min-h-0 flex-1">
        <div
          className={cx('relative min-w-0 flex-1 overflow-auto', dragging && 'bg-accent-soft')}
          onDragOver={(e) => {
            if (!canAdd || results) return
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
          {dropOnly && !results ? (
            <EmptyState
              icon={<Upload className="size-7" />}
              title={`Add files to ${crumbs.length ? crumbs[crumbs.length - 1] : share.name}`}
              action={<Button variant="primary" icon={<Upload className="size-3.5" />} disabled={!canAdd} onClick={() => fileInput.current?.click()}>Upload</Button>}
            >
              {share.shared_by} shared this folder for adding files: drag files here or use Upload. What is already in it is not shown,
              and nothing is replaced: a file with the same name gets a new one.
            </EmptyState>
          ) : listing.error && !results ? (
            <div className="p-6">
              <ErrorNote>{listing.error.message}</ErrorNote>
              {listing.error.status === 404 && (
                <Button className="mt-3" size="sm" onClick={() => go({ path: base })}>Go to {share ? share.name : root?.name}</Button>
              )}
            </div>
          ) : items.length === 0 ? (
            listing.loading ? (
              <div className="p-6"><Spinner /></div>
            ) : (
              <EmptyState icon={<Upload className="size-7" />} title={results ? 'No matches' : 'This folder is empty'}>
                {!results && canAdd && 'Drag files here or use Upload.'}
                {!results && fullRoot?.scanning && ' Indexing is still in progress.'}
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
                  {results && <th className={cx('hidden w-64 font-medium', narrow ? 'xl:table-cell' : 'md:table-cell')}>Location</th>}
                  <th className="w-20 pr-3 font-medium md:w-28 md:pr-0"><SortHeader label="Size" field="size" sort={sort} order={order} onSort={onSort} className="ml-auto" /></th>
                  <th className={cx('hidden w-44 pl-6 font-medium', narrow ? 'lg:table-cell' : 'md:table-cell')}><SortHeader label="Modified" field="modified" sort={sort} order={order} onSort={onSort} /></th>
                  <th className={cx('hidden w-24 pr-4 font-medium', narrow ? 'xl:table-cell' : 'md:table-cell')}><SortHeader label="Type" field="type" sort={sort} order={order} onSort={onSort} /></th>
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
                        <td className={cx('hidden truncate pr-3 text-muted', narrow ? 'xl:table-cell' : 'md:table-cell')}>
                          {(f as Found).device_name ? `${(f as Found).device_name} · ` : ''}{(f as FoundItem).root_name}{f.parent_path ? ` / ${f.parent_path}` : ''}
                        </td>
                      )}
                      <td className="pr-3 text-right text-muted tabular md:pr-0">{f.is_dir ? '' : formatBytes(f.size)}</td>
                      <td className={cx('hidden truncate pl-6 text-muted', narrow ? 'lg:table-cell' : 'md:table-cell')}>{formatDate(f.modified_at)}</td>
                      <td className={cx('hidden truncate pr-4 text-muted', narrow ? 'xl:table-cell' : 'md:table-cell')}>{fileKind(f)}</td>
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
                  <Thumb file={f} prefix={prefixOf(f)} />
                  <div className="mt-2 truncate text-[12.5px]">{f.filename}</div>
                  <div className="truncate text-[11.5px] text-subtle">
                    {(f as Found).device_name ? `${(f as Found).device_name} · ` : ''}{f.is_dir ? 'Folder' : formatBytes(f.size)}
                  </div>
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
          <Details
            file={single}
            rootName={(single as FoundItem).root_name ?? root?.name ?? ''}
            fapi={fileApi(prefixOf(single))}
            writable={writable}
            onChanged={() => void listing.reload()}
            onClose={() => setShowDetails(false)}
          />
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
        {fullRoot?.scanning && <span className="text-warn">Indexing…</span>}
        <span className="ml-auto">{touch ? 'Tap to open · press and hold to select' : 'Double-click to open · Del to delete · F2 to rename'}</span>
      </div>

      <FilePreview
        file={preview}
        siblings={preview ? items.filter((i) => deviceOf(i) === deviceOf(preview)) : items}
        onNavigate={setPreview}
        onClose={() => setPreview(null)}
        prefix={preview ? prefixOf(preview) : prefix}
      />
      {sharing && <ShareDialog file={sharing} prefix={prefixOf(sharing)} onClose={() => setSharing(null)} />}
      {fetching && root && (
        <FetchDialog fapi={fapi} rootId={root.id} path={path} where={`${where}${root.name}${path ? ` / ${path}` : ''}`} onClose={() => setFetching(false)} />
      )}
      {encrypting && <EncryptDialog folder={encrypting} onClose={() => setEncrypting(null)} />}
      {asking && rootId && (
        <AskForFilesDialog root_id={rootId} path={asking.relative_path} name={asking.filename} onClose={() => setAsking(null)} />
      )}
      {sharingFolder && (
        <ShareFolderDialog
          target={{ root_id: sharingFolder.root_id, path: sharingFolder.relative_path, name: sharingFolder.filename }}
          onClose={() => setSharingFolder(null)}
        />
      )}
      {copying && selectedItems.length > 0 && (
        <CopyDialog
          items={selectedItems}
          fromDevice={deviceOf(selectedItems[0])}
          fromOf={deviceOf}
          devices={peers}
          onClose={() => setCopying(false)}
          onDone={() => {
            setCopying(false)
            setSelected(new Set())
            void listing.reload()
          }}
        />
      )}
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

/** Copies the selected files to a folder on another device (or this one). */
function CopyDialog({ items, fromDevice, fromOf, devices, onClose, onDone }: {
  items: FileEntry[]
  /** Device the files live on; '' for this device. */
  fromDevice: string
  /** The device each file lives on (results can span devices). */
  fromOf: (f: FileEntry) => string
  devices: Device[]
  onClose: () => void
  onDone: () => void
}) {
  // Places to copy to: every other device, and this one unless it is a phone
  // (a phone has no storage of its own).
  const places = [
    ...(isMobile() ? [] : [{ id: '', name: `This ${thisDevice()}`, online: true }]),
    ...devices.map((d) => ({ id: d.id, name: d.name, online: d.online })),
  ]
  const [to, setTo] = useState(() => (places.find((p) => p.id !== fromDevice && p.online) ?? places[0])?.id ?? '')
  const prefix = to ? peerPrefix(to) : ''
  const roots = useResource(async () => ({ to, list: (await fileApi(prefix).roots()).filter((r) => r.online && !r.read_only) }), [to])
  const rootList = roots.data?.to === to ? roots.data.list : undefined
  const [rootId, setRootId] = useState('')
  const [dest, setDest] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const activeRoot = rootList?.find((r) => r.id === rootId) ?? rootList?.[0]

  useEffect(() => {
    setRootId('')
    setDest('')
  }, [to])

  const copy = async () => {
    if (!activeRoot) return
    setBusy(true)
    setError('')
    // Search results can come from several devices: one copy per source.
    const bySource = new Map<string, string[]>()
    for (const f of items) bySource.set(fromOf(f), [...(bySource.get(fromOf(f)) ?? []), f.id])
    const failures: string[] = []
    for (const [from, ids] of bySource) {
      try {
        await api.copyItems({ from_device: from || undefined, file_ids: ids, to_device: to || undefined, root_id: activeRoot.id, path: dest })
      } catch (e) {
        failures.push((e as Error).message)
      }
    }
    setBusy(false)
    if (failures.length === bySource.size) {
      setError(failures.join('\n'))
      return
    }
    const place = places.find((p) => p.id === to)?.name ?? 'device'
    toast(`Copying ${what} to ${place}. Follow it in Transfers.`)
    onDone()
  }

  const what = items.length === 1 ? items[0].filename : `${items.length} items`
  return (
    <Modal
      open
      onClose={busy ? () => {} : onClose}
      title={`Copy ${what}`}
      footer={
        <>
          <Button variant="ghost" disabled={busy} onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!activeRoot} onClick={() => void copy()}>
            Copy here
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Field label="To device">
            <select
              value={to}
              disabled={busy}
              onChange={(e) => setTo(e.target.value)}
              className="h-8 w-full rounded-md border border-line bg-bg px-2 text-fg outline-none focus:border-accent"
            >
              {places.map((p) => (
                <option key={p.id} value={p.id} disabled={!p.online}>{p.name}{p.online ? '' : ' (offline)'}</option>
              ))}
            </select>
          </Field>
          <Field label="Storage">
            <select
              value={activeRoot?.id ?? ''}
              disabled={busy || !rootList?.length}
              onChange={(e) => { setRootId(e.target.value); setDest('') }}
              className="h-8 w-full rounded-md border border-line bg-bg px-2 text-fg outline-none focus:border-accent"
            >
              {rootList?.map((r) => <option key={r.id} value={r.id}>{r.name}</option>)}
            </select>
          </Field>
        </div>
        {roots.error ? (
          <ErrorNote>{roots.error.message}</ErrorNote>
        ) : !rootList ? (
          <Spinner />
        ) : !activeRoot ? (
          <p className="text-muted">That device has no storage you can write to.</p>
        ) : (
          <FolderPicker rootId={activeRoot.id} value={dest} onChange={setDest} prefix={prefix} />
        )}
        <p className="text-[12px] text-subtle">
          The original stays where it is. If something with the same name exists, the copy gets a number added. The copy runs in the background and picks up where it stopped if the connection drops.
        </p>
        {error && <ErrorNote><span className="whitespace-pre-wrap">{error}</span></ErrorNote>}
      </div>
    </Modal>
  )
}

function FileDialogs({ fapi, kind, root, path, items, onClose, onDone }: {
  fapi: FileApi
  kind: null | 'folder' | 'rename' | 'move' | 'delete'
  root: RootSummary
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

/** Downloads a web link straight into the folder being browsed. */
function FetchDialog({ fapi, rootId, path, where, onClose }: { fapi: FileApi; rootId: string; path: string; where: string; onClose: () => void }) {
  const [url, setUrl] = useState('')
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const submit = async () => {
    setBusy(true)
    try {
      await fapi.fetchUrl({ root_id: rootId, path, url: url.trim(), name: name.trim() || undefined })
      toast('Downloading — follow it under Transfers')
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
      onClose={onClose}
      title="Download from a link"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!/^https?:\/\/./i.test(url.trim()) || busy} onClick={submit}>Download</Button>
        </>
      }
    >
      <p className="mb-3 text-[13px] text-muted">The device downloads the file itself, into {where}. You can close the app meanwhile.</p>
      <Field label="Link"><Input autoFocus value={url} placeholder="https://" onChange={(e) => setUrl(e.target.value)} /></Field>
      <Field label="Save as (optional)"><Input value={name} placeholder="Name from the link" onChange={(e) => setName(e.target.value)} /></Field>
    </Modal>
  )
}
