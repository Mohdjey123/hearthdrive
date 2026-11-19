import { useState } from 'react'
import { FolderOpen, HardDrive, Usb } from 'lucide-react'
import { api, isTauri, type Drive, type StorageRoot } from '../services/api'
import { useResource } from '../hooks/useResource'
import { formatBytes } from '../lib/format'
import { Button, ErrorNote, Field, Input, ProgressBar, Spinner, cx } from './ui'

async function pickFolder(): Promise<string | null> {
  const { open } = await import('@tauri-apps/plugin-dialog')
  const picked = await open({ directory: true, multiple: false, title: 'Choose a folder for Hearthdrive' })
  return typeof picked === 'string' ? picked : null
}

export function DriveList({ selected, onSelect }: { selected?: string; onSelect: (d: Drive) => void }) {
  const drives = useResource(() => api.drives(), [])
  if (drives.loading && !drives.data) return <Spinner />
  if (!drives.data?.length) return <p className="text-muted">No drives detected.</p>
  return (
    <div className="grid grid-cols-2 gap-2">
      {drives.data.map((d) => {
        const used = d.total_bytes - d.available_bytes
        const Icon = d.removable ? Usb : HardDrive
        return (
          <button
            key={d.mount_point}
            type="button"
            onClick={() => onSelect(d)}
            className={cx(
              'rounded-md border p-3 text-left transition-colors',
              selected === d.mount_point ? 'border-accent bg-accent-soft' : 'border-line bg-elevated hover:border-line-strong',
            )}
          >
            <div className="flex items-center gap-2">
              <Icon className="size-4 text-muted" />
              <span className="font-medium">{d.mount_point}</span>
              <span className="truncate text-muted">{d.label}</span>
            </div>
            <ProgressBar className="mt-2.5" value={d.total_bytes ? used / d.total_bytes : 0} tone={d.available_bytes / d.total_bytes < 0.1 ? 'warn' : 'accent'} />
            <div className="mt-1.5 text-[11.5px] text-subtle tabular">
              {formatBytes(d.available_bytes)} free of {formatBytes(d.total_bytes)}
              {d.removable && ' · removable'}
            </div>
          </button>
        )
      })}
    </div>
  )
}

/** Choose a folder or drive and register it as a storage root. */
export function AddStorageForm({ onAdded, submitLabel = 'Add storage' }: { onAdded: (root: StorageRoot) => void; submitLabel?: string }) {
  const [path, setPath] = useState('')
  const [name, setName] = useState('')
  const [readOnly, setReadOnly] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setError('')
    try {
      const root = await api.addRoot({ path: path.trim(), name: name.trim() || undefined, read_only: readOnly })
      setPath('')
      setName('')
      setReadOnly(false)
      onAdded(root)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <form onSubmit={submit} className="space-y-4">
      <div className="space-y-1.5">
        <span className="block text-[12px] font-medium text-muted">Detected drives</span>
        <DriveList selected={path} onSelect={(d) => setPath(d.mount_point)} />
      </div>
      <Field label="Folder or drive" hint="Hearthdrive indexes this location in place. Nothing is moved or copied.">
        <div className="flex gap-2">
          <Input value={path} onChange={(e) => setPath(e.target.value)} placeholder="D:\Cloud" spellCheck={false} />
          {isTauri() && (
            <Button
              type="button"
              icon={<FolderOpen className="size-3.5" />}
              onClick={async () => {
                const p = await pickFolder()
                if (p) setPath(p)
              }}
            >
              Browse
            </Button>
          )}
        </div>
      </Field>
      <Field label="Display name (optional)">
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="Media library" />
      </Field>
      <label className="flex items-center gap-2 text-[12.5px] text-muted">
        <input type="checkbox" checked={readOnly} onChange={(e) => setReadOnly(e.target.checked)} className="accent-[var(--nx-accent)]" />
        Read-only — files can be viewed and downloaded but not changed
      </label>
      <ErrorNote>{error}</ErrorNote>
      <div className="flex justify-end">
        <Button type="submit" variant="primary" loading={busy} disabled={!path.trim()}>
          {submitLabel}
        </Button>
      </div>
    </form>
  )
}
