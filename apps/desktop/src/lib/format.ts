const UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']

export function formatBytes(bytes: number | null | undefined, digits = 1): string {
  if (bytes === null || bytes === undefined || Number.isNaN(bytes)) return '—'
  if (bytes < 1024) return `${bytes} B`
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024
    unit++
  }
  return `${value.toFixed(value >= 100 ? 0 : digits)} ${UNITS[unit]}`
}

export function formatCount(n: number): string {
  return new Intl.NumberFormat().format(n)
}

const dateFmt = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' })

export function formatDate(ms: number | null | undefined): string {
  if (!ms) return '—'
  return dateFmt.format(new Date(ms))
}

const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' })

export function formatRelative(ms: number | null | undefined): string {
  if (!ms) return '—'
  const diff = (ms - Date.now()) / 1000
  const abs = Math.abs(diff)
  if (abs < 45) return 'just now'
  if (abs < 3600) return rtf.format(Math.round(diff / 60), 'minute')
  if (abs < 86400) return rtf.format(Math.round(diff / 3600), 'hour')
  if (abs < 86400 * 30) return rtf.format(Math.round(diff / 86400), 'day')
  return formatDate(ms)
}

export function formatDuration(ms: number): string {
  const s = Math.floor(ms / 1000)
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m`
  const h = Math.floor(m / 60)
  if (h < 48) return `${h}h ${m % 60}m`
  return `${Math.floor(h / 24)}d ${h % 24}h`
}

export function fileKind(entry: { is_dir: boolean; mime_type: string | null; extension: string | null }): string {
  if (entry.is_dir) return 'Folder'
  const mime = entry.mime_type ?? ''
  if (mime.startsWith('image/')) return 'Image'
  if (mime.startsWith('video/')) return 'Video'
  if (mime.startsWith('audio/')) return 'Audio'
  if (mime === 'application/pdf') return 'PDF'
  if (mime.startsWith('text/')) return 'Text'
  if (['zip', '7z', 'rar', 'tar', 'gz'].includes(entry.extension ?? '')) return 'Archive'
  if (entry.extension === 'nxlocked') return 'Encrypted'
  return entry.extension ? entry.extension.toUpperCase() : 'File'
}

/** Camera raw and HEIC photos: shown through their preview. */
const RAW_EXT = new Set(['dng', 'cr2', 'cr3', 'crw', 'nef', 'nrw', 'arw', 'srf', 'sr2', 'orf', 'rw2', 'raf', 'srw', 'pef', 'x3f', '3fr', 'erf', 'kdc', 'mrw', 'iiq', 'rwl', 'heic', 'heif'])

export type PreviewKind = 'image' | 'video' | 'audio' | 'text' | 'pdf' | 'archive' | null

const TEXT_EXT = new Set(['txt', 'md', 'json', 'csv', 'log', 'xml', 'yml', 'yaml', 'toml', 'ini', 'ts', 'tsx', 'js', 'rs', 'py', 'css', 'html', 'sql', 'sh', 'ps1', 'bat'])

export function previewKind(entry: { is_dir: boolean; mime_type: string | null; extension: string | null }): PreviewKind {
  if (entry.is_dir) return null
  const mime = entry.mime_type ?? ''
  if (mime.startsWith('image/') && mime !== 'image/svg+xml') return 'image'
  if (RAW_EXT.has((entry.extension ?? '').toLowerCase())) return 'image'
  if (mime.startsWith('video/')) return 'video'
  if (mime.startsWith('audio/')) return 'audio'
  if (mime === 'application/pdf' || entry.extension === 'pdf') return 'pdf'
  if (['zip', 'cbz', '7z', 'cb7', 'tar', 'tgz', 'gz'].includes(entry.extension ?? '')) return 'archive'
  if (mime.startsWith('text/') || TEXT_EXT.has(entry.extension ?? '')) return 'text'
  return null
}

export function joinPath(parent: string, name: string) {
  return parent ? `${parent}/${name}` : name
}
