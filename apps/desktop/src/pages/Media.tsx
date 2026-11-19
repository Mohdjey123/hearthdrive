import { useEffect, useMemo, useRef, useState } from 'react'
import { Clapperboard, Disc3, Film, Home, ListVideo, Music, Pause, Play, SkipBack, SkipForward, Tv } from 'lucide-react'
import {
  api, contentUrl, isMobile, libraryApi, peerPrefix, thisDevice,
  type FileEntry, type LibraryItem, type MediaInfo, type MusicAlbum, type Playback,
} from '../services/api'
import { useResource } from '../hooks/useResource'
import { navigate, type Route } from '../hooks/useRoute'
import { formatRelative } from '../lib/format'
import { toastError } from '../components/Toasts'
import { EmptyState, ErrorNote, IconButton, Modal, PageHeader, Spinner, cx } from '../components/ui'

type Tab = 'continue' | 'films' | 'series' | 'music' | 'home'
const TABS: { id: Tab; label: string; icon: typeof Film }[] = [
  { id: 'continue', label: 'Continue', icon: ListVideo },
  { id: 'films', label: 'Films', icon: Film },
  { id: 'series', label: 'Series', icon: Tv },
  { id: 'music', label: 'Music', icon: Music },
  { id: 'home', label: 'Home videos', icon: Home },
]

/** Containers browsers play themselves; the rest are converted (with ffmpeg). */
const PLAYABLE = new Set(['mp4', 'm4v', 'webm', 'mov', 'ogv', 'mp3', 'm4a', 'aac', 'flac', 'wav', 'ogg', 'oga', 'opus'])

function duration(ms: number | null | undefined) {
  if (!ms) return ''
  const s = Math.round(ms / 1000)
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  return h ? `${h}:${String(m).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}` : `${m}:${String(s % 60).padStart(2, '0')}`
}

function titleOf(f: { filename: string; media: MediaInfo }) {
  const m = f.media
  if (m.media_type === 'episode' && m.show_name) return `${m.show_name} · S${String(m.season ?? 0).padStart(2, '0')}E${String(m.episode ?? 0).padStart(2, '0')}`
  if (m.title) return m.year && m.media_type === 'film' ? `${m.title} (${m.year})` : m.title
  return f.filename.replace(/\.[^.]+$/, '')
}

function Progress({ p }: { p: Playback | null | undefined }) {
  if (!p || !p.duration_ms) return null
  return (
    <div className="h-1 w-full overflow-hidden rounded bg-line">
      <div className="h-full bg-accent" style={{ width: `${Math.min(100, (p.position_ms / p.duration_ms) * 100)}%` }} />
    </div>
  )
}

/** Plays a video, continuing where this person stopped on any device. */
function Player({ prefix, item, onClose }: { prefix: string; item: (FileEntry & { media: MediaInfo }) | null; onClose: () => void }) {
  const lib = libraryApi(prefix)
  const ref = useRef<HTMLVideoElement>(null)
  const [src, setSrc] = useState<string | null>(null)
  const [error, setError] = useState('')
  const [converted, setConverted] = useState(false)
  const [resumeAt, setResumeAt] = useState(0)
  const ext = (item?.extension ?? '').toLowerCase()

  useEffect(() => {
    setSrc(null)
    setError('')
    if (!item) return
    let live = true
    ;(async () => {
      const [caps, saved] = await Promise.all([lib.capabilities().catch(() => null), lib.playback(item.id).catch(() => null)])
      const resume = saved && saved.position_ms > 10000 && (!saved.duration_ms || saved.position_ms < saved.duration_ms * 0.95) ? saved.position_ms / 1000 : 0
      if (!live) return
      setResumeAt(resume)
      if (PLAYABLE.has(ext) || !caps?.transcode) {
        if (!PLAYABLE.has(ext)) setError('This browser may not play this format. Install ffmpeg on the computer that holds it, and NEXUS converts it while it plays.')
        setConverted(false)
        setSrc(await contentUrl(item.id, false, prefix))
      } else {
        const { url } = await lib.streamLink(item.id)
        setConverted(true)
        // Converted streams cannot seek: start where the viewer stopped.
        setSrc(lib.url(`${url}${resume ? `&start=${resume}` : ''}`))
      }
    })().catch((e) => live && setError(e instanceof Error ? e.message : String(e)))
    return () => {
      live = false
    }
  }, [item?.id, prefix])

  // Remember the place every few seconds and when leaving.
  useEffect(() => {
    if (!item || !src) return
    const save = () => {
      const v = ref.current
      if (!v || !Number.isFinite(v.currentTime)) return
      const position = (converted ? resumeAt : 0) + v.currentTime
      const total = item.media.duration_ms ?? (Number.isFinite(v.duration) ? v.duration * 1000 : undefined)
      void lib.savePlayback(item.id, { position_ms: Math.round(position * 1000), duration_ms: total ? Math.round(total) : undefined, device: thisDevice() }).catch(() => {})
    }
    const t = setInterval(save, 10000)
    return () => {
      clearInterval(t)
      save()
    }
  }, [item?.id, src])

  return (
    <Modal open={!!item} onClose={onClose} wide title={item ? titleOf(item) : ''}>
      {error && <div className="mb-2"><ErrorNote>{error}</ErrorNote></div>}
      {!src ? (
        <div className="flex min-h-[30vh] items-center justify-center"><Spinner /></div>
      ) : (
        <video
          ref={ref}
          src={src}
          controls
          autoPlay
          className="max-h-[70vh] w-full rounded bg-black"
          onLoadedMetadata={(e) => {
            if (!converted && resumeAt) e.currentTarget.currentTime = resumeAt
          }}
        />
      )}
      {resumeAt > 0 && <p className="mt-2 text-[12px] text-subtle">Continuing at {duration(resumeAt * 1000)}{converted ? ' (converted while it plays)' : ''}.</p>}
    </Modal>
  )
}

function VideoGrid({ items, onPlay }: { items: LibraryItem[] | (FileEntry & { media: MediaInfo; playback: Playback | null })[]; onPlay: (i: FileEntry & { media: MediaInfo }) => void }) {
  return (
    <div className="grid grid-cols-[repeat(auto-fill,minmax(200px,1fr))] gap-3">
      {items.map((it) => (
        <button key={it.id} onClick={() => onPlay(it)} className="flex flex-col gap-1.5 rounded-lg border border-line bg-panel p-3 text-left hover:border-line-strong">
          <div className="flex items-center gap-2">
            <Clapperboard className="size-4 shrink-0 text-accent" />
            <span className="truncate font-medium">{titleOf(it)}</span>
          </div>
          <span className="truncate text-[12px] text-subtle">
            {[duration(it.media.duration_ms), it.playback?.device_name && `last on ${it.playback.device_name}`, it.playback && formatRelative(it.playback.updated_at)].filter(Boolean).join(' · ') || it.filename}
          </span>
          <Progress p={it.playback} />
        </button>
      ))}
    </div>
  )
}

function Series({ prefix, onPlay }: { prefix: string; onPlay: (i: FileEntry & { media: MediaInfo }) => void }) {
  const lib = libraryApi(prefix)
  const episodes = useResource(() => lib.items('episode'), [prefix])
  const [show, setShow] = useState<string | null>(null)
  const shows = useMemo(() => {
    const m = new Map<string, LibraryItem[]>()
    for (const e of episodes.data ?? []) m.set(e.media.show_name ?? 'Unknown', [...(m.get(e.media.show_name ?? 'Unknown') ?? []), e])
    return [...m.entries()]
  }, [episodes.data])
  if (episodes.loading && !episodes.data) return <Spinner />
  if (!shows.length) return <EmptyState icon={<Tv className="size-6" />} title="No series yet">Episodes named like Show.S01E02 or in Season folders show up here.</EmptyState>
  if (show) {
    const eps = shows.find(([s]) => s === show)?.[1] ?? []
    const seasons = [...new Set(eps.map((e) => e.media.season ?? 0))]
    return (
      <div>
        <button className="mb-3 text-[13px] text-muted hover:text-fg" onClick={() => setShow(null)}>← Series</button>
        <h2 className="mb-3 text-[16px] font-semibold">{show}</h2>
        {seasons.map((s) => (
          <section key={s} className="mb-5">
            <h3 className="mb-2 text-[13px] font-semibold text-muted">Season {s}</h3>
            <VideoGrid items={eps.filter((e) => (e.media.season ?? 0) === s)} onPlay={onPlay} />
          </section>
        ))}
      </div>
    )
  }
  return (
    <div className="grid grid-cols-[repeat(auto-fill,minmax(200px,1fr))] gap-3">
      {shows.map(([name, eps]) => (
        <button key={name} onClick={() => setShow(name)} className="rounded-lg border border-line bg-panel p-4 text-left hover:border-line-strong">
          <Tv className="mb-2 size-5 text-accent" />
          <div className="truncate font-medium">{name}</div>
          <div className="text-[12px] text-subtle">{eps.length} episode{eps.length === 1 ? '' : 's'} · {new Set(eps.map((e) => e.media.season)).size} season{new Set(eps.map((e) => e.media.season)).size === 1 ? '' : 's'}</div>
        </button>
      ))}
    </div>
  )
}

/** Albums, their songs, and a small player that plays one after another. */
function MusicLibrary({ prefix }: { prefix: string }) {
  const lib = libraryApi(prefix)
  const albums = useResource(() => lib.music(), [prefix])
  const [album, setAlbum] = useState<MusicAlbum | null>(null)
  const tracks = useResource(() => (album ? lib.items('music', { album: album.album, artist: album.artist }) : Promise.resolve([] as LibraryItem[])), [album, prefix])
  const [queue, setQueue] = useState<LibraryItem[]>([])
  const [index, setIndex] = useState(0)
  const [src, setSrc] = useState<string | null>(null)
  const audio = useRef<HTMLAudioElement>(null)
  const [playing, setPlaying] = useState(false)
  const current = queue[index]

  useEffect(() => {
    if (!current) return
    let live = true
    contentUrl(current.id, false, prefix).then((u) => live && setSrc(u)).catch(toastError)
    return () => {
      live = false
    }
  }, [current?.id])

  const playFrom = (list: LibraryItem[], i: number) => {
    setQueue(list)
    setIndex(i)
  }

  if (albums.loading && !albums.data) return <Spinner />
  if (!albums.data?.length) return <EmptyState icon={<Music className="size-6" />} title="No music yet">Songs in your storage show up here by album and artist, from their tags.</EmptyState>
  return (
    <div className={cx(current && 'pb-20')}>
      {album ? (
        <div>
          <button className="mb-3 text-[13px] text-muted hover:text-fg" onClick={() => setAlbum(null)}>← Albums</button>
          <h2 className="text-[16px] font-semibold">{album.album || 'Unknown album'}</h2>
          <div className="mb-3 text-[12.5px] text-muted">{album.artist || 'Unknown artist'}{album.year ? ` · ${album.year}` : ''} · {album.tracks} songs · {duration(album.duration_ms)}</div>
          {!tracks.data ? <Spinner /> : (
            <ol className="divide-y divide-line rounded-lg border border-line">
              {tracks.data.map((t, i) => (
                <li key={t.id}>
                  <button onClick={() => playFrom(tracks.data!, i)} className={cx('flex w-full items-center gap-3 px-3 py-2 text-left hover:bg-elevated', current?.id === t.id && 'text-accent')}>
                    <span className="w-6 text-right text-[12px] text-subtle tabular">{t.media.track ?? i + 1}</span>
                    <span className="flex-1 truncate">{t.media.title ?? t.filename}</span>
                    <span className="text-[12px] text-subtle tabular">{duration(t.media.duration_ms)}</span>
                  </button>
                </li>
              ))}
            </ol>
          )}
        </div>
      ) : (
        <div className="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-3">
          {albums.data.map((a) => (
            <button key={`${a.artist}/${a.album}`} onClick={() => setAlbum(a)} className="rounded-lg border border-line bg-panel p-3 text-left hover:border-line-strong">
              <Disc3 className="mb-2 size-8 text-accent" />
              <div className="truncate font-medium">{a.album || 'Unknown album'}</div>
              <div className="truncate text-[12px] text-subtle">{a.artist || 'Unknown artist'}{a.year ? ` · ${a.year}` : ''}</div>
            </button>
          ))}
        </div>
      )}
      {current && (
        <div className="fixed inset-x-0 bottom-0 z-30 flex items-center gap-3 border-t border-line bg-panel px-4 py-2 md:left-56">
          <IconButton label="Previous" disabled={index === 0} onClick={() => setIndex(index - 1)}><SkipBack className="size-4" /></IconButton>
          <IconButton label={playing ? 'Pause' : 'Play'} onClick={() => (audio.current?.paused ? void audio.current.play() : audio.current?.pause())}>
            {playing ? <Pause className="size-4" /> : <Play className="size-4" />}
          </IconButton>
          <IconButton label="Next" disabled={index >= queue.length - 1} onClick={() => setIndex(index + 1)}><SkipForward className="size-4" /></IconButton>
          <div className="min-w-0 flex-1">
            <div className="truncate text-[13px] font-medium">{current.media.title ?? current.filename}</div>
            <div className="truncate text-[12px] text-subtle">{current.media.artist ?? ''}</div>
          </div>
          {src && (
            <audio
              ref={audio}
              src={src}
              autoPlay
              controls
              className="h-8 w-64 max-w-[40vw]"
              onPlay={() => setPlaying(true)}
              onPause={() => setPlaying(false)}
              onEnded={() => (index < queue.length - 1 ? setIndex(index + 1) : setPlaying(false))}
            />
          )}
        </div>
      )}
    </div>
  )
}

/** Films, series, music and home videos on this device or a paired one. */
export function Media({ route }: { route: Route }) {
  const deviceId = route.params.get('device') ?? (isMobile() ? null : '')
  const tab = (route.params.get('tab') as Tab) || 'continue'
  const devices = useResource(() => api.devices().catch(() => []), [])
  const peers = (devices.data ?? []).filter((d) => !d.is_local && d.online)
  const prefix = deviceId ? peerPrefix(deviceId) : ''
  const lib = libraryApi(prefix)
  const [playing, setPlaying] = useState<(FileEntry & { media: MediaInfo }) | null>(null)
  const go = (t: Tab, dev = deviceId) => navigate('media', { tab: t === 'continue' ? undefined : t, device: dev || undefined })

  const cont = useResource(() => (tab === 'continue' && deviceId !== null ? lib.continueWatching() : Promise.resolve([])), [tab, prefix])
  const films = useResource(() => (tab === 'films' && deviceId !== null ? lib.items('film') : Promise.resolve([])), [tab, prefix])
  const home = useResource(() => (tab === 'home' && deviceId !== null ? lib.items('home_video') : Promise.resolve([])), [tab, prefix])

  return (
    <div>
      <PageHeader
        title="Media"
        subtitle={deviceId ? `On ${(devices.data ?? []).find((d) => d.id === deviceId)?.name ?? 'a paired device'}` : `On this ${thisDevice()}`}
        actions={
          (peers.length > 0 || isMobile()) && (
            <select className="h-8 rounded-md border border-line bg-panel px-2 text-[12.5px]" value={deviceId ?? ''} onChange={(e) => go(tab, e.target.value)}>
              {!isMobile() && <option value="">This {thisDevice()}</option>}
              {isMobile() && deviceId === null && <option value="">Choose a computer</option>}
              {peers.map((d) => <option key={d.id} value={d.id}>{d.name}</option>)}
            </select>
          )
        }
      />
      <div className="mb-4 flex gap-1 overflow-x-auto border-b border-line">
        {TABS.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            onClick={() => go(id)}
            className={cx('flex h-9 shrink-0 items-center gap-1.5 border-b-2 px-3 text-[13px]', tab === id ? 'border-accent text-fg' : 'border-transparent text-muted hover:text-fg')}
          >
            <Icon className="size-3.5" /> {label}
          </button>
        ))}
      </div>
      {deviceId === null ? (
        <EmptyState icon={<Film className="size-6" />} title="Choose a computer">Pick a paired computer above to see its films, series and music.</EmptyState>
      ) : tab === 'continue' ? (
        cont.loading && !cont.data ? <Spinner /> : cont.data?.length ? (
          <VideoGrid items={cont.data} onPlay={setPlaying} />
        ) : (
          <EmptyState icon={<ListVideo className="size-6" />} title="Nothing to continue">Films and episodes you start show up here, on every device you sign in to with the same name.</EmptyState>
        )
      ) : tab === 'films' ? (
        films.loading && !films.data ? <Spinner /> : films.data?.length ? <VideoGrid items={films.data} onPlay={setPlaying} /> : (
          <EmptyState icon={<Film className="size-6" />} title="No films yet">Videos named like "Film Name (2019)" show up here.</EmptyState>
        )
      ) : tab === 'series' ? (
        <Series prefix={prefix} onPlay={setPlaying} />
      ) : tab === 'music' ? (
        <MusicLibrary prefix={prefix} />
      ) : home.loading && !home.data ? <Spinner /> : home.data?.length ? <VideoGrid items={home.data} onPlay={setPlaying} /> : (
        <EmptyState icon={<Home className="size-6" />} title="No home videos yet">Other videos in your storage show up here.</EmptyState>
      )}
      <Player prefix={prefix} item={playing} onClose={() => { setPlaying(null); cont.reload() }} />
    </div>
  )
}
