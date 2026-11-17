import {
  File, FileArchive, FileAudio, FileCode, FileImage, FileSpreadsheet, FileText, FileVideo, Folder,
} from 'lucide-react'
import { cx } from './ui'

const CODE = new Set(['js', 'ts', 'tsx', 'jsx', 'rs', 'py', 'json', 'html', 'css', 'toml', 'yml', 'yaml', 'xml', 'sh', 'ps1', 'sql', 'c', 'cpp', 'h', 'go', 'java', 'kt'])
const SHEET = new Set(['xls', 'xlsx', 'csv', 'ods'])
const ARCHIVE = new Set(['zip', '7z', 'rar', 'tar', 'gz', 'bz2', 'xz', 'iso'])
const DOC = new Set(['doc', 'docx', 'odt', 'rtf', 'txt', 'md', 'pdf'])

export function FileIcon({ entry, className }: {
  entry: { is_dir: boolean; mime_type: string | null; extension: string | null }
  className?: string
}) {
  const ext = entry.extension ?? ''
  const mime = entry.mime_type ?? ''
  const base = cx('size-4 shrink-0', className)
  if (entry.is_dir) return <Folder className={cx(base, 'text-accent')} fill="currentColor" fillOpacity={0.18} />
  if (mime.startsWith('image/')) return <FileImage className={cx(base, 'text-info')} />
  if (mime.startsWith('video/')) return <FileVideo className={cx(base, 'text-[#b48ad6]')} />
  if (mime.startsWith('audio/')) return <FileAudio className={cx(base, 'text-warn')} />
  if (ARCHIVE.has(ext)) return <FileArchive className={cx(base, 'text-muted')} />
  if (SHEET.has(ext)) return <FileSpreadsheet className={cx(base, 'text-accent')} />
  if (CODE.has(ext)) return <FileCode className={cx(base, 'text-muted')} />
  if (DOC.has(ext) || mime.startsWith('text/')) return <FileText className={cx(base, 'text-muted')} />
  return <File className={cx(base, 'text-subtle')} />
}
