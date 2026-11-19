import { lazy, Suspense, useEffect, useState } from 'react'
import { ChevronLeft, ChevronRight, Download, Tag } from 'lucide-react'
import { contentUrl, fileApi, isMobile, libraryApi, type ArchiveListing, type DocText, type FileEntry, type FileTag } from '../services/api'
import { toast, toastError } from './Toasts'
import { formatBytes, formatDate, previewKind } from '../lib/format'
import { FileIcon } from './FileIcon'
import { Button, IconButton, Modal, Spinner, cx } from './ui'

const PdfViewer = lazy(() => import('./PdfViewer').then((m) => ({ default: m.PdfViewer })))

/** What is inside a .zip, .7z or .tar file. */
function ArchivePreview({ file, prefix }: { file: FileEntry; prefix: string }) {
  const [listing, setListing] = useState<ArchiveListing | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    let live = true
    setListing(null)
    setError(null)
    fileApi(prefix).archive(file.id).then((l) => live && setListing(l)).catch((e) => live && setError(e.message))
    return () => { live = false }
  }, [file.id, prefix])
  if (error) return <div className="text-center text-muted">{error}</div>
  if (!listing) return <Spinner />
  return (
    <div className="w-full">
      <div className="mb-2 text-[12.5px] text-muted">
        {listing.total_entries.toLocaleString()} item{listing.total_entries === 1 ? '' : 's'} · {formatBytes(listing.total_size)} unpacked ·{' '}
        {listing.format}{listing.truncated && ` · showing the first ${listing.entries.length.toLocaleString()}`}
      </div>
      <div className="max-h-[60vh] overflow-auto rounded-md border border-line">
        <table className="w-full text-left text-[12.5px]">
          <tbody>
            {listing.entries.map((e, i) => (
              <tr key={i} className="border-b border-line last:border-0">
                <td className="truncate px-3 py-1 font-mono">{e.path}{e.is_dir ? '/' : ''}</td>
                <td className="w-24 px-3 py-1 text-right text-muted">{e.is_dir ? '' : formatBytes(e.size)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

const urlCache = new Map<string, { url: string; at: number }>()

/** Cached signed content URL (tickets last hours; refresh after one). */
export async function cachedContentUrl(id: string, prefix = '') {
  const key = `${prefix}|${id}`
  const hit = urlCache.get(key)
  if (hit && Date.now() - hit.at < 60 * 60 * 1000) return hit.url
  const url = await contentUrl(id, false, prefix)
  urlCache.set(key, { url, at: Date.now() })
  return url
}

export async function downloadFile(file: FileEntry, prefix = '') {
  await saveUrl(await contentUrl(file.id, true, prefix), file.filename)
}

/** Saves what a signed URL serves as a file named `filename`. */
export async function saveUrl(url: string, filename: string) {
  if (isMobile()) {
    // A web view cannot save files on a phone; the app writes the file to
    // the phone's Download folder instead.
    const { invoke } = await import('@tauri-apps/api/core')
    toast(`Downloading ${filename}…`)
    const saved = await invoke<string>('save_download', { url, filename })
    toast(`Saved to ${saved}`)
    return
  }
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  a.rel = 'noopener'
  document.body.appendChild(a)
  a.click()
  a.remove()
}

export function useContentUrl(file: FileEntry | null, enabled = true, prefix = '') {
  const [url, setUrl] = useState<string | null>(null)
  useEffect(() => {
    setUrl(null)
    if (!file || !enabled) return
    let live = true
    cachedContentUrl(file.id, prefix).then((u) => live && setUrl(u)).catch(() => {})
    return () => {
      live = false
    }
  }, [file?.id, enabled, prefix])
  return url
}

function TextPreview({ url }: { url: string }) {
  const [text, setText] = useState<string | null>(null)
  useEffect(() => {
    const ctrl = new AbortController()
    fetch(url, { headers: { Range: 'bytes=0-262143' }, signal: ctrl.signal })
      .then((r) => r.text())
      .then(setText)
      .catch(() => setText('Could not load a preview.'))
    return () => ctrl.abort()
  }, [url])
  if (text === null) return <Spinner />
  return (
    <pre className="max-h-[65vh] overflow-auto rounded-md bg-bg p-4 font-mono text-[12px] leading-relaxed whitespace-pre-wrap text-fg">
      {text}
    </pre>
  )
}

/** Tags on a file, and what HearthDrive read from it. */
export function TagBar({ file, prefix = '' }: { file: FileEntry; prefix?: string }) {
  const lib = libraryApi(prefix)
  const [tags, setTags] = useState<FileTag[] | null>(null)
  const [text, setText] = useState<DocText | null>(null)
  const [adding, setAdding] = useState('')
  useEffect(() => {
    setTags(null)
    setText(null)
    lib.fileTags(file.id).then((r) => { setTags(r.tags); setText(r.text) }).catch(() => setTags([]))
  }, [file.id, prefix])
  const save = async (next: FileTag[]) => {
    try {
      const r = await lib.setFileTags(file.id, next.filter((t) => !t.auto).map((t) => t.tag), next.filter((t) => t.auto).map((t) => t.tag))
      setTags(r.tags)
    } catch (e) {
      toastError(e)
    }
  }
  if (!tags) return null
  const add = () => {
    const t = adding.trim()
    setAdding('')
    if (t && !tags.some((x) => x.tag.toLowerCase() === t.toLowerCase())) void save([...tags, { tag: t, auto: false, added_by: '', added_at: 0 }])
  }
  const how: Record<string, string> = { text: 'text', pdf: 'PDF text', office: 'document text', ocr: 'text recognised on the pages' }
  return (
    <div className="flex flex-wrap items-center gap-1.5 text-[12px]">
      <Tag className="size-3.5 text-subtle" />
      {tags.map((t) => (
        <span key={t.tag} className={cx('flex items-center gap-1 rounded-full border px-2 py-0.5', t.auto ? 'border-dashed border-line text-muted' : 'border-line bg-elevated')} title={t.auto ? 'Given by HearthDrive from what the document says' : `Added by ${t.added_by}`}>
          {t.tag}
          <button aria-label={`Remove ${t.tag}`} className="text-subtle hover:text-danger" onClick={() => void save(tags.filter((x) => x.tag !== t.tag))}>×</button>
        </span>
      ))}
      <input
        value={adding}
        onChange={(e) => setAdding(e.target.value)}
        onKeyDown={(e) => { if (e.key === 'Enter') add() }}
        onBlur={add}
        placeholder="Add a tag"
        className="h-6 w-24 rounded border border-transparent bg-transparent px-1 outline-none hover:border-line focus:border-accent"
      />
      {text && text.chars > 0 && <span className="ml-auto text-subtle">Searchable: {text.chars.toLocaleString()} characters of {how[text.method ?? ''] ?? 'text'}</span>}
    </div>
  )
}

/** Photos browsers cannot show (camera raw, HEIC): their preview is shown. */
export function needsConversion(file: { extension: string | null }) {
  return CONVERTED.has((file.extension ?? '').toLowerCase())
}
const CONVERTED = new Set(['dng', 'cr2', 'cr3', 'crw', 'nef', 'nrw', 'arw', 'srf', 'sr2', 'orf', 'rw2', 'raf', 'srw', 'pef', 'x3f', '3fr', 'erf', 'kdc', 'mrw', 'iiq', 'rwl', 'heic', 'heif', 'tif', 'tiff'])

export function FilePreview({ file, siblings, onNavigate, onClose, prefix = '' }: {
  file: FileEntry | null
  siblings: FileEntry[]
  onNavigate: (f: FileEntry) => void
  onClose: () => void
  prefix?: string
}) {
  const kind = file ? previewKind(file) : null
  const url = useContentUrl(file, kind !== null && kind !== 'archive', prefix)
  const previewable = siblings.filter((s) => previewKind(s) !== null)
  const index = file ? previewable.findIndex((s) => s.id === file.id) : -1
  const prev = index > 0 ? previewable[index - 1] : null
  const next = index >= 0 && index < previewable.length - 1 ? previewable[index + 1] : null

  useEffect(() => {
    if (!file) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'ArrowLeft' && prev) onNavigate(prev)
      if (e.key === 'ArrowRight' && next) onNavigate(next)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [file, prev, next, onNavigate])

  return (
    <Modal
      open={!!file}
      onClose={onClose}
      wide
      title={
        file && (
          <span className="flex items-center gap-2">
            <FileIcon entry={file} /> {file.filename}
            <span className="font-normal text-subtle">
              {formatBytes(file.size)} · {formatDate(file.modified_at)}
            </span>
          </span>
        )
      }
      footer={
        file && (
          <div className="flex w-full flex-col gap-2">
          <TagBar file={file} prefix={prefix} />
          <div className="flex w-full items-center justify-between">
            <div className="flex gap-1">
              <IconButton label="Previous" disabled={!prev} onClick={() => prev && onNavigate(prev)}>
                <ChevronLeft className="size-4" />
              </IconButton>
              <IconButton label="Next" disabled={!next} onClick={() => next && onNavigate(next)}>
                <ChevronRight className="size-4" />
              </IconButton>
            </div>
            <Button icon={<Download className="size-3.5" />} onClick={() => void downloadFile(file, prefix).catch(toastError)}>
              Download
            </Button>
          </div>
          </div>
        )
      }
    >
      {file && (
        <div className="flex min-h-[40vh] items-center justify-center">
          {kind === null ? (
            <div className="text-center text-muted">
              <FileIcon entry={file} className="mx-auto mb-3 size-10" />
              No preview for this file type.
            </div>
          ) : kind === 'archive' ? (
            <ArchivePreview file={file} prefix={prefix} />
          ) : !url ? (
            <Spinner />
          ) : kind === 'pdf' ? (
            <Suspense fallback={<Spinner />}><PdfViewer url={url} /></Suspense>
          ) : kind === 'image' ? (
            <img src={needsConversion(file) ? url.replace('/content?', '/thumbnail?size=1024&') : url} alt={file.filename} className="max-h-[68vh] max-w-full rounded object-contain" />
          ) : kind === 'video' ? (
            <video src={url} controls autoPlay className="max-h-[68vh] max-w-full rounded bg-black" />
          ) : kind === 'audio' ? (
            <audio src={url} controls autoPlay className="w-full" />
          ) : (
            <div className="w-full"><TextPreview url={url} /></div>
          )}
        </div>
      )}
    </Modal>
  )
}

/** Folder chooser for moves, limited to one storage root. */
export function FolderPicker({ rootId, value, onChange, exclude, prefix = '' }: {
  rootId: string
  value: string
  onChange: (path: string) => void
  exclude?: string[]
  prefix?: string
}) {
  const [folders, setFolders] = useState<FileEntry[] | null>(null)
  useEffect(() => {
    setFolders(null)
    fileApi(prefix)
      .list({ root_id: rootId, path: value, limit: 1000 })
      .then((r) => setFolders(r.items.filter((i) => i.is_dir && !exclude?.includes(i.relative_path))))
      .catch(() => setFolders([]))
  }, [rootId, value, exclude?.join('|'), prefix])
  const parts = value ? value.split('/') : []
  return (
    <div className="rounded-md border border-line">
      <div className="flex flex-wrap items-center gap-1 border-b border-line px-3 py-2 text-[12.5px]">
        <button className="text-accent hover:underline" onClick={() => onChange('')}>Root</button>
        {parts.map((p, i) => (
          <span key={i} className="flex items-center gap-1">
            <ChevronRight className="size-3 text-subtle" />
            <button className="hover:underline" onClick={() => onChange(parts.slice(0, i + 1).join('/'))}>{p}</button>
          </span>
        ))}
      </div>
      <div className="h-56 overflow-auto py-1">
        {folders === null ? (
          <div className="p-3"><Spinner /></div>
        ) : folders.length === 0 ? (
          <div className="p-3 text-muted">No subfolders</div>
        ) : (
          folders.map((f) => (
            <button
              key={f.id}
              onClick={() => onChange(f.relative_path)}
              className="flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-hover"
            >
              <FileIcon entry={f} /> {f.filename}
            </button>
          ))
        )}
      </div>
    </div>
  )
}
