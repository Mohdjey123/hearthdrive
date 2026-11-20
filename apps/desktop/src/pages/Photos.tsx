import { useEffect, useMemo, useRef, useState } from 'react'
import { Calendar, Camera, Check, Copy, FolderPlus, Images, MapPin, Plus, Share2, Sparkles, Trash2, Video } from 'lucide-react'
import {
  api, fileApi, isMobile, peerPrefix, photoApi, thisDevice,
  type FileEntry, type PhotoAlbum, type PhotoItem, type PhotoPlace, type PhotoQuery,
} from '../services/api'
import { useResource } from '../hooks/useResource'
import { navigate, type Route } from '../hooks/useRoute'
import { formatBytes, formatDate } from '../lib/format'
import { FilePreview } from '../components/FilePreview'
import { toast, toastError } from '../components/Toasts'
import { PhoneStorageEmpty, usePhoneStorage } from '../components/PhoneStorage'
import { Badge, Button, EmptyState, ErrorNote, Input, Modal, PageHeader, Progress, Spinner, cx } from '../components/ui'
import { runEach, type EachProgress } from '../lib/runEach'

type Tab = 'timeline' | 'videos' | 'albums' | 'places' | 'memories' | 'similar'
const TABS: { id: Tab; label: string; icon: typeof Images }[] = [
  { id: 'timeline', label: 'Photos', icon: Images },
  { id: 'videos', label: 'Videos', icon: Video },
  { id: 'albums', label: 'Albums', icon: FolderPlus },
  { id: 'places', label: 'Places', icon: MapPin },
  { id: 'memories', label: 'Memories', icon: Sparkles },
  { id: 'similar', label: 'Look-alikes', icon: Copy },
]

const PAGE = 240

function monthLabel(month: string) {
  const [y, m] = month.split('-').map(Number)
  return new Date(Date.UTC(y, m - 1, 1)).toLocaleDateString(undefined, { month: 'long', year: 'numeric', timeZone: 'UTC' })
}

/** The month (YYYY-MM) a photo belongs to: taken dates are local times stored as UTC. */
function monthOf(p: PhotoItem) {
  const t = p.media.taken_at ?? p.modified_at ?? 0
  const d = new Date(t)
  return `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, '0')}`
}

function Thumb({ item, url, selected, onOpen, onSelect, selecting }: {
  item: PhotoItem
  url: string | null
  selected?: boolean
  selecting?: boolean
  onOpen: () => void
  onSelect?: (on: boolean) => void
}) {
  return (
    <div className="group relative aspect-square overflow-hidden rounded-md bg-elevated">
      <button
        className="block size-full"
        onClick={() => (selecting && onSelect ? onSelect(!selected) : onOpen())}
        title={`${item.filename}${item.media.camera ? ` · ${item.media.camera}` : ''}`}
      >
        {url ? (
          <img src={url} alt={item.filename} loading="lazy" decoding="async" className="size-full object-cover" />
        ) : (
          <span className="flex size-full items-center justify-center p-2 text-center text-[11px] text-subtle">
            {item.media.kind === 'video' ? <Video className="size-6" /> : item.filename}
          </span>
        )}
        {item.media.kind === 'video' && item.media.duration_ms ? (
          <span className="absolute right-1 bottom-1 rounded bg-black/60 px-1 text-[10.5px] text-white tabular">
            {Math.floor(item.media.duration_ms / 60000)}:{String(Math.floor(item.media.duration_ms / 1000) % 60).padStart(2, '0')}
          </span>
        ) : null}
      </button>
      {onSelect && (
        <button
          aria-label={selected ? 'Unselect' : 'Select'}
          onClick={() => onSelect(!selected)}
          className={cx(
            'absolute top-1.5 left-1.5 flex size-5 items-center justify-center rounded-full border border-white/80 shadow',
            selected ? 'bg-accent text-white' : 'bg-black/25 opacity-0 group-hover:opacity-100',
            selecting && 'opacity-100',
          )}
        >
          {selected && <Check className="size-3" />}
        </button>
      )}
    </div>
  )
}

/** Photos and videos newest first, by month, loading more on scroll. */
function Timeline({ prefix, kind, onOpen, selected, setSelected }: {
  prefix: string
  kind: 'photo' | 'video'
  onOpen: (item: PhotoItem, all: PhotoItem[]) => void
  selected: Set<string>
  setSelected: (s: Set<string>) => void
}) {
  const papi = photoApi(prefix)
  const [camera, setCamera] = useState('')
  const [people, setPeople] = useState(false)
  const overview = useResource(() => papi.months({ kind }), [prefix, kind], (e) => e.type === 'storage.scan.completed')
  const [items, setItems] = useState<PhotoItem[]>([])
  const [more, setMore] = useState(true)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')
  const sentinel = useRef<HTMLDivElement>(null)
  const query: PhotoQuery = { kind, camera: camera || undefined, people: people ? 1 : undefined }

  const load = async (reset: boolean) => {
    if (loading) return
    setLoading(true)
    try {
      const page = await papi.list({ ...query, offset: reset ? 0 : items.length, limit: PAGE })
      setItems((old) => (reset ? page.items : [...old, ...page.items]))
      setMore(page.more)
      setError('')
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    setItems([])
    setMore(true)
    void load(true)
  }, [prefix, kind, camera, people])

  useEffect(() => {
    const el = sentinel.current
    if (!el) return
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting) && more && !loading) void load(false)
    }, { rootMargin: '800px' })
    io.observe(el)
    return () => io.disconnect()
  })

  const groups = useMemo(() => {
    const out: { month: string; items: PhotoItem[] }[] = []
    for (const it of items) {
      const m = monthOf(it)
      const last = out[out.length - 1]
      if (last && last.month === m) last.items.push(it)
      else out.push({ month: m, items: [it] })
    }
    return out
  }, [items])

  const counts = new Map((overview.data?.months ?? []).map((m) => [m.month, m.count]))
  const toggle = (id: string, on: boolean) => {
    const n = new Set(selected)
    if (on) n.add(id)
    else n.delete(id)
    setSelected(n)
  }

  return (
    <div>
      <div className="mb-3 flex flex-wrap items-center gap-2 text-[12.5px]">
        {(overview.data?.cameras.length ?? 0) > 1 && (
          <select className="h-8 rounded-md border border-line bg-panel px-2" value={camera} onChange={(e) => setCamera(e.target.value)}>
            <option value="">Every camera</option>
            {overview.data!.cameras.map(([c, n]) => <option key={c} value={c}>{c} ({n.toLocaleString()})</option>)}
          </select>
        )}
        {kind === 'photo' && (
          <label className="flex items-center gap-1.5">
            <input type="checkbox" className="accent-[var(--nx-accent)]" checked={people} onChange={(e) => setPeople(e.target.checked)} />
            With people
          </label>
        )}
        {overview.data?.reading && <span className="flex items-center gap-1.5 text-subtle"><Spinner className="size-3.5" /> Reading new photos…</span>}
      </div>
      {error && <ErrorNote>{error}</ErrorNote>}
      {!loading && items.length === 0 && !error ? (
        <EmptyState icon={kind === 'video' ? <Video className="size-6" /> : <Images className="size-6" />} title={kind === 'video' ? 'No videos yet' : 'No photos yet'}>
          {kind === 'video'
            ? 'Videos in your storage show up here.'
            : 'Photos in your storage show up here with their dates, cameras and places, once HearthDrive has read them.'}
        </EmptyState>
      ) : (
        groups.map((g) => (
          <section key={g.month} className="mb-6">
            <h2 className="mb-2 flex items-baseline gap-2 text-[13px] font-semibold">
              <Calendar className="size-3.5 self-center text-muted" /> {monthLabel(g.month)}
              {counts.get(g.month) ? <span className="font-normal text-subtle">{counts.get(g.month)!.toLocaleString()}</span> : null}
            </h2>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(120px,1fr))] gap-1.5">
              {g.items.map((it) => (
                <Thumb
                  key={it.id}
                  item={it}
                  url={papi.url(it.thumb_url)}
                  selected={selected.has(it.id)}
                  selecting={selected.size > 0}
                  onSelect={(on) => toggle(it.id, on)}
                  onOpen={() => onOpen(it, items)}
                />
              ))}
            </div>
          </section>
        ))
      )}
      <div ref={sentinel} className="h-8">{loading && <Spinner />}</div>
    </div>
  )
}

function AlbumDialog({ prefix, open, onClose, fileIds }: { prefix: string; open: boolean; onClose: (changed: boolean) => void; fileIds: string[] }) {
  const papi = photoApi(prefix)
  const albums = useResource(() => (open ? papi.albums() : Promise.resolve([] as PhotoAlbum[])), [open, prefix])
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const addTo = async (id: string | null) => {
    setBusy(true)
    try {
      if (id) await papi.changeAlbum(id, { add: fileIds })
      else await papi.createAlbum(name.trim(), fileIds)
      toast(`Added ${fileIds.length} item${fileIds.length === 1 ? '' : 's'} to ${id ? albums.data?.find((a) => a.id === id)?.name : name.trim()}`)
      onClose(true)
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal open={open} onClose={() => onClose(false)} title={`Add ${fileIds.length} to an album`}>
      <div className="space-y-3">
        <div className="flex gap-2">
          <Input placeholder="New album name" value={name} onChange={(e) => setName(e.target.value)} />
          <Button variant="primary" disabled={!name.trim()} loading={busy} onClick={() => void addTo(null)}>Create</Button>
        </div>
        <ul className="max-h-72 divide-y divide-line overflow-auto">
          {(albums.data ?? []).filter((a) => a.can_add).map((a) => (
            <li key={a.id}>
              <button className="flex w-full items-center gap-3 py-2 text-left hover:text-accent" disabled={busy} onClick={() => void addTo(a.id)}>
                <span className="size-9 shrink-0 overflow-hidden rounded bg-elevated">
                  {a.cover_url && <img src={papi.url(a.cover_url)!} alt="" className="size-full object-cover" />}
                </span>
                <span className="flex-1 truncate">{a.name}</span>
                <span className="text-[12px] text-subtle">{a.items}</span>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </Modal>
  )
}

function ShareAlbum({ prefix, album, onClose }: { prefix: string; album: PhotoAlbum; onClose: () => void }) {
  const papi = photoApi(prefix)
  const people = useResource(() => (prefix ? Promise.resolve([]) : api.users().catch(() => [])), [prefix])
  const devices = useResource(() => (prefix ? Promise.resolve([]) : api.devices().catch(() => [])), [prefix])
  const [members, setMembers] = useState(album.members)
  const [busy, setBusy] = useState(false)
  const has = (m: string) => members.find((x) => x.member === m)
  const set = (m: string, on: boolean, canAdd = false) =>
    setMembers((old) => (on ? [...old.filter((x) => x.member !== m), { member: m, can_add: canAdd }] : old.filter((x) => x.member !== m)))
  const save = async () => {
    setBusy(true)
    try {
      await papi.changeAlbum(album.id, { members })
      toast(members.length ? `${album.name} is shared with ${members.length}` : `${album.name} is no longer shared`)
      onClose()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  const rows = [
    ...(people.data ?? []).map((u) => ({ id: `user:${u.id}`, label: u.username, hint: 'Person' })),
    ...(devices.data ?? []).filter((d) => !d.is_local).map((d) => ({ id: `device:${d.id}`, label: d.name, hint: 'Paired device' })),
  ]
  return (
    <Modal open onClose={onClose} title={`Share ${album.name}`} footer={<><Button variant="ghost" onClick={onClose}>Cancel</Button><Button variant="primary" loading={busy} onClick={() => void save()}>Save</Button></>}>
      <p className="mb-3 text-[12.5px] text-muted">People you share an album with see its photos, even those in storage they cannot otherwise see. Nothing else is shared.</p>
      {prefix && <p className="mb-3 text-[12.5px] text-subtle">Share albums from the device that holds them.</p>}
      <ul className="divide-y divide-line">
        {rows.map((r) => (
          <li key={r.id} className="flex items-center gap-3 py-2">
            <input type="checkbox" className="accent-[var(--nx-accent)]" checked={!!has(r.id)} onChange={(e) => set(r.id, e.target.checked)} />
            <span className="flex-1 truncate">{r.label} <span className="text-[12px] text-subtle">· {r.hint}</span></span>
            {has(r.id) && (
              <label className="flex items-center gap-1.5 text-[12px] text-muted">
                <input type="checkbox" className="accent-[var(--nx-accent)]" checked={has(r.id)!.can_add} onChange={(e) => set(r.id, true, e.target.checked)} /> Can add
              </label>
            )}
          </li>
        ))}
      </ul>
    </Modal>
  )
}

function Albums({ prefix, onOpen }: { prefix: string; onOpen: (item: PhotoItem, all: PhotoItem[]) => void }) {
  const papi = photoApi(prefix)
  const albums = useResource(() => papi.albums(), [prefix])
  const [openId, setOpenId] = useState<string | null>(null)
  const album = useResource(() => (openId ? papi.album(openId) : Promise.resolve(null)), [openId, prefix])
  const [sharing, setSharing] = useState<PhotoAlbum | null>(null)
  const [name, setName] = useState('')

  const create = async () => {
    try {
      await papi.createAlbum(name.trim())
      setName('')
      albums.reload()
    } catch (e) {
      toastError(e)
    }
  }
  const remove = async (a: PhotoAlbum) => {
    try {
      await papi.deleteAlbum(a.id)
      toast(`Deleted the album ${a.name}. Its photos are where they were.`)
      setOpenId(null)
      albums.reload()
    } catch (e) {
      toastError(e)
    }
  }

  if (openId && album.data) {
    const a = album.data
    return (
      <div>
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <Button size="sm" variant="ghost" onClick={() => setOpenId(null)}>← Albums</Button>
          <h2 className="text-[15px] font-semibold">{a.name}</h2>
          <span className="text-[12px] text-subtle">{a.items.length} item{a.items.length === 1 ? '' : 's'}</span>
          {a.members.length > 0 && <Badge>Shared with {a.members.length}</Badge>}
          <div className="ml-auto flex gap-2">
            {a.mine && <Button size="sm" icon={<Share2 className="size-3.5" />} onClick={() => setSharing(a)}>Share</Button>}
            {a.mine && <Button size="sm" variant="ghost" className="hover:text-danger" icon={<Trash2 className="size-3.5" />} onClick={() => void remove(a)}>Delete album</Button>}
          </div>
        </div>
        {a.items.length === 0 ? (
          <EmptyState icon={<Images className="size-6" />} title="This album is empty">Select photos under Photos and choose Add to album.</EmptyState>
        ) : (
          <div className="grid grid-cols-[repeat(auto-fill,minmax(140px,1fr))] gap-1.5">
            {a.items.map((it) => <Thumb key={it.id} item={it} url={papi.url(it.thumb_url)} onOpen={() => onOpen(it, a.items)} />)}
          </div>
        )}
        {sharing && <ShareAlbum prefix={prefix} album={sharing} onClose={() => { setSharing(null); album.reload() }} />}
      </div>
    )
  }

  return (
    <div>
      <div className="mb-4 flex max-w-md gap-2">
        <Input placeholder="New album" value={name} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && name.trim() && void create()} />
        <Button icon={<Plus className="size-3.5" />} disabled={!name.trim()} onClick={() => void create()}>Create</Button>
      </div>
      {albums.loading && !albums.data ? (
        <Spinner />
      ) : !albums.data?.length ? (
        <EmptyState icon={<FolderPlus className="size-6" />} title="No albums yet">Create one here, or select photos and add them to an album.</EmptyState>
      ) : (
        <div className="grid grid-cols-[repeat(auto-fill,minmax(170px,1fr))] gap-3">
          {albums.data.map((a) => (
            <button key={a.id} onClick={() => setOpenId(a.id)} className="text-left">
              <div className="aspect-square overflow-hidden rounded-lg bg-elevated">
                {a.cover_url ? <img src={papi.url(a.cover_url)!} alt="" className="size-full object-cover" /> : <Images className="m-auto mt-[40%] size-8 text-subtle" />}
              </div>
              <div className="mt-1.5 truncate font-medium">{a.name}</div>
              <div className="text-[12px] text-subtle">
                {a.items} item{a.items === 1 ? '' : 's'}{!a.mine ? ' · shared with you' : a.members.length ? ' · shared' : ''}
              </div>
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

/** Photos by place: a plot of where they were taken, and places as groups. */
function Places({ prefix, onOpenId }: { prefix: string; onOpenId: (fileId: string) => void }) {
  const papi = photoApi(prefix)
  const places = useResource(() => papi.places(), [prefix])
  const groups = useMemo(() => {
    const m = new Map<string, PhotoPlace[]>()
    for (const p of places.data ?? []) {
      // About 10 km cells.
      const key = `${Math.round(p.latitude * 10) / 10},${Math.round(p.longitude * 10) / 10}`
      m.set(key, [...(m.get(key) ?? []), p])
    }
    return [...m.entries()].sort((a, b) => b[1].length - a[1].length)
  }, [places.data])
  if (places.loading && !places.data) return <Spinner />
  if (!places.data?.length) {
    return <EmptyState icon={<MapPin className="size-6" />} title="No places yet">Photos taken with location turned on show up here by where they were taken.</EmptyState>
  }
  const W = 720
  const H = 360
  return (
    <div>
      <div className="mb-4 overflow-hidden rounded-lg border border-line bg-elevated">
        <svg viewBox={`0 0 ${W} ${H}`} className="block w-full" role="img" aria-label="Where photos were taken">
          {[-60, -30, 0, 30, 60].map((lat) => (
            <line key={lat} x1={0} x2={W} y1={((90 - lat) / 180) * H} y2={((90 - lat) / 180) * H} stroke="var(--nx-line)" strokeWidth={lat === 0 ? 1.2 : 0.6} />
          ))}
          {[-120, -60, 0, 60, 120].map((lon) => (
            <line key={lon} y1={0} y2={H} x1={((lon + 180) / 360) * W} x2={((lon + 180) / 360) * W} stroke="var(--nx-line)" strokeWidth={lon === 0 ? 1.2 : 0.6} />
          ))}
          {groups.map(([key, ps]) => {
            const [lat, lon] = key.split(',').map(Number)
            return (
              <circle key={key} cx={((lon + 180) / 360) * W} cy={((90 - lat) / 180) * H} r={Math.min(14, 3 + Math.sqrt(ps.length))} fill="var(--nx-accent)" fillOpacity={0.55}>
                <title>{ps.length} photo{ps.length === 1 ? '' : 's'} near {lat.toFixed(1)}, {lon.toFixed(1)}</title>
              </circle>
            )
          })}
        </svg>
      </div>
      <div className="space-y-5">
        {groups.slice(0, 60).map(([key, ps]) => {
          const [lat, lon] = key.split(',').map(Number)
          return (
            <section key={key}>
              <div className="mb-1.5 flex items-center gap-2 text-[13px]">
                <MapPin className="size-3.5 text-accent" />
                <span className="font-medium tabular">{Math.abs(lat).toFixed(1)}° {lat >= 0 ? 'N' : 'S'}, {Math.abs(lon).toFixed(1)}° {lon >= 0 ? 'E' : 'W'}</span>
                <span className="text-subtle">{ps.length} photo{ps.length === 1 ? '' : 's'}</span>
                <a
                  className="ml-auto text-[12px] text-muted underline decoration-line-strong underline-offset-2 hover:text-fg"
                  href={`https://www.openstreetmap.org/?mlat=${lat}&mlon=${lon}#map=11/${lat}/${lon}`}
                  target="_blank"
                  rel="noreferrer"
                >
                  Open map
                </a>
              </div>
              <div className="flex gap-1.5 overflow-x-auto pb-1">
                {ps.slice(0, 20).map((p) => (
                  <button key={p.file_id} className="size-20 shrink-0 overflow-hidden rounded bg-elevated" onClick={() => onOpenId(p.file_id)}>
                    <img src={papi.url(p.thumb_url)!} alt="" loading="lazy" className="size-full object-cover" />
                  </button>
                ))}
              </div>
            </section>
          )
        })}
      </div>
      <p className="mt-4 text-[12px] text-subtle">The map above is drawn here, without loading anything from the internet. Open map shows the place on OpenStreetMap in your browser.</p>
    </div>
  )
}

function Memories({ prefix, onOpen }: { prefix: string; onOpen: (item: PhotoItem, all: PhotoItem[]) => void }) {
  const papi = photoApi(prefix)
  const memories = useResource(() => papi.memories(), [prefix])
  if (memories.loading && !memories.data) return <Spinner />
  if (!memories.data?.length) {
    return <EmptyState icon={<Sparkles className="size-6" />} title="Nothing from this day yet">Photos taken on today's date in earlier years show up here.</EmptyState>
  }
  return (
    <div className="space-y-6">
      {memories.data.map((m) => (
        <section key={m.year}>
          <h2 className="mb-2 text-[14px] font-semibold">
            {m.years_ago === 1 ? 'One year ago' : `${m.years_ago} years ago`} <span className="font-normal text-subtle">· {m.year}</span>
          </h2>
          <div className="grid grid-cols-[repeat(auto-fill,minmax(140px,1fr))] gap-1.5">
            {m.items.map((it) => <Thumb key={it.id} item={it} url={papi.url(it.thumb_url)} onOpen={() => onOpen(it, m.items)} />)}
          </div>
        </section>
      ))}
    </div>
  )
}

function Similar({ prefix, onOpen }: { prefix: string; onOpen: (item: PhotoItem, all: PhotoItem[]) => void }) {
  const papi = photoApi(prefix)
  const groups = useResource(() => papi.similar(), [prefix])
  const [busy, setBusy] = useState<string | null>(null)
  const [progress, setProgress] = useState<EachProgress | null>(null)
  const keepFirst = async (g: PhotoItem[]) => {
    setBusy(g[0].id)
    const r = await runEach(g.slice(1), (it) => fileApi(prefix).deleteFile(it.id), {
      size: (it) => it.size,
      name: (it) => it.filename,
      onProgress: setProgress,
    })
    setBusy(null)
    setProgress(null)
    const done = r.ok.length
    if (r.failed.length) toastError(r.failed[0].error)
    if (done) toast(`Moved ${done} look-alike${done === 1 ? '' : 's'} to the trash`)
    groups.reload()
  }
  if (groups.loading && !groups.data) return <Spinner />
  if (!groups.data?.length) {
    return <EmptyState icon={<Copy className="size-6" />} title="No look-alikes found">Bursts, edits and copies of a photo in other sizes show up here as groups.</EmptyState>
  }
  return (
    <div className="space-y-5">
      <p className="text-[12.5px] text-muted">Photos that look alike: bursts, edited copies and the same photo in other sizes. The most detailed is first. Exact copies are under Storage → Find duplicates.</p>
      {groups.data.map((g) => (
        <section key={g[0].id} className="rounded-lg border border-line p-3">
          <div className="mb-2 flex items-center gap-2 text-[12.5px]">
            <span className="font-medium">{g.length} photos</span>
            <span className="text-subtle">{formatDate(g[0].media.taken_at ?? g[0].modified_at)}</span>
            <Button size="sm" className="ml-auto" variant="ghost" loading={busy === g[0].id} disabled={busy !== null} onClick={() => void keepFirst(g)}>Keep the first, trash the rest</Button>
          </div>
          {busy === g[0].id && progress && (
            <Progress className="mb-2" label="Moving to the trash" done={progress.done} total={progress.total} bytesDone={progress.bytesDone} bytesTotal={progress.bytesTotal} noun="photos" />
          )}
          <div className="flex gap-1.5 overflow-x-auto">
            {g.map((it, i) => (
              <div key={it.id} className="w-28 shrink-0">
                <Thumb item={it} url={papi.url(it.thumb_url)} onOpen={() => onOpen(it, g)} />
                <div className="mt-0.5 truncate text-[11px] text-subtle">
                  {i === 0 ? 'Keep · ' : ''}{it.media.width}×{it.media.height} · {formatBytes(it.size)}
                </div>
              </div>
            ))}
          </div>
        </section>
      ))}
    </div>
  )
}

/** Photos and videos on this device or a paired one. */
export function Photos({ route }: { route: Route }) {
  // On a phone, `device=here` is the phone itself; with no device it opens
  // the phone once its storage is shown, else asks for a computer.
  const phone = usePhoneStorage()
  const local = !isMobile() || !!phone.data?.root_id
  const raw = route.params.get('device')
  const deviceId = raw === 'here' ? '' : raw ?? (local ? '' : null)
  const tab = (route.params.get('tab') as Tab) || 'timeline'
  const devices = useResource(() => api.devices().catch(() => []), [])
  const peers = (devices.data ?? []).filter((d) => !d.is_local && d.online)
  const prefix = deviceId ? peerPrefix(deviceId) : ''
  const [open, setOpen] = useState<{ item: FileEntry; all: FileEntry[] } | null>(null)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [adding, setAdding] = useState(false)
  useEffect(() => setSelected(new Set()), [prefix, tab])

  const go = (t: Tab, dev = deviceId) =>
    navigate('photos', { tab: t === 'timeline' ? undefined : t, device: dev || (dev === '' && isMobile() ? 'here' : undefined) })
  const openItem = (item: PhotoItem, all: PhotoItem[]) => setOpen({ item, all })
  const openId = async (fileId: string) => {
    try {
      const f = await fileApi(prefix).file(fileId)
      setOpen({ item: f, all: [f] })
    } catch (e) {
      toastError(e)
    }
  }

  return (
    <div>
      <PageHeader
        title="Photos"
        subtitle={deviceId ? `On ${(devices.data ?? []).find((d) => d.id === deviceId)?.name ?? 'a paired device'}` : deviceId === '' ? `On this ${thisDevice()}` : 'On your computers'}
        actions={
          <>
            {selected.size > 0 && (
              <>
                <span className="text-[12.5px] text-muted">{selected.size} selected</span>
                <Button size="sm" variant="ghost" onClick={() => setSelected(new Set())}>Clear</Button>
                <Button size="sm" variant="primary" icon={<FolderPlus className="size-3.5" />} onClick={() => setAdding(true)}>Add to album</Button>
              </>
            )}
            {(peers.length > 0 || isMobile()) && (
              <select
                className="h-8 rounded-md border border-line bg-panel px-2 text-[12.5px]"
                value={deviceId ?? '-'}
                onChange={(e) => e.target.value !== '-' && go(tab, e.target.value)}
              >
                {isMobile() && deviceId === null && <option value="-">Choose a computer</option>}
                <option value="">This {thisDevice()}</option>
                {peers.map((d) => <option key={d.id} value={d.id}>{d.name}</option>)}
              </select>
            )}
          </>
        }
      />
      <div className="mb-4 flex gap-1 overflow-x-auto border-b border-line">
        {TABS.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            onClick={() => go(id)}
            className={cx(
              'flex h-9 shrink-0 items-center gap-1.5 border-b-2 px-3 text-[13px]',
              tab === id ? 'border-accent text-fg' : 'border-transparent text-muted hover:text-fg',
            )}
          >
            <Icon className="size-3.5" /> {label}
          </button>
        ))}
      </div>
      {deviceId === null ? (
        phone.loading ? <Spinner /> : <EmptyState icon={<Camera className="size-6" />} title="Choose a computer">Pick a paired computer above to see its photos.</EmptyState>
      ) : deviceId === '' && !local ? (
        <PhoneStorageEmpty onShown={phone.reload} />
      ) : tab === 'timeline' || tab === 'videos' ? (
        <Timeline key={`${prefix}-${tab}`} prefix={prefix} kind={tab === 'videos' ? 'video' : 'photo'} onOpen={openItem} selected={selected} setSelected={setSelected} />
      ) : tab === 'albums' ? (
        <Albums prefix={prefix} onOpen={openItem} />
      ) : tab === 'places' ? (
        <Places prefix={prefix} onOpenId={(id) => void openId(id)} />
      ) : tab === 'memories' ? (
        <Memories prefix={prefix} onOpen={openItem} />
      ) : (
        <Similar prefix={prefix} onOpen={openItem} />
      )}
      <FilePreview
        file={open?.item ?? null}
        siblings={open?.all ?? []}
        onNavigate={(f) => setOpen((o) => (o ? { ...o, item: f } : o))}
        onClose={() => setOpen(null)}
        prefix={prefix}
      />
      <AlbumDialog prefix={prefix} open={adding} fileIds={[...selected]} onClose={(changed) => { setAdding(false); if (changed) setSelected(new Set()) }} />
    </div>
  )
}
