import { useEffect, useState } from 'react'
import { ChevronLeft, ChevronRight, Download } from 'lucide-react'
import { contentUrl, fileApi, type FileEntry } from '../services/api'
import { formatBytes, formatDate, previewKind } from '../lib/format'
import { FileIcon } from './FileIcon'
import { Button, IconButton, Modal, Spinner } from './ui'

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
  const url = await contentUrl(file.id, true, prefix)
  const a = document.createElement('a')
  a.href = url
  a.download = file.filename
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

export function FilePreview({ file, siblings, onNavigate, onClose, prefix = '' }: {
  file: FileEntry | null
  siblings: FileEntry[]
  onNavigate: (f: FileEntry) => void
  onClose: () => void
  prefix?: string
}) {
  const kind = file ? previewKind(file) : null
  const url = useContentUrl(file, kind !== null, prefix)
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
          <div className="flex w-full items-center justify-between">
            <div className="flex gap-1">
              <IconButton label="Previous" disabled={!prev} onClick={() => prev && onNavigate(prev)}>
                <ChevronLeft className="size-4" />
              </IconButton>
              <IconButton label="Next" disabled={!next} onClick={() => next && onNavigate(next)}>
                <ChevronRight className="size-4" />
              </IconButton>
            </div>
            <Button icon={<Download className="size-3.5" />} onClick={() => void downloadFile(file, prefix)}>
              Download
            </Button>
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
          ) : !url ? (
            <Spinner />
          ) : kind === 'image' ? (
            <img src={url} alt={file.filename} className="max-h-[68vh] max-w-full rounded object-contain" />
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
