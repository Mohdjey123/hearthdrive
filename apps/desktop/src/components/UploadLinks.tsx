import { useState } from 'react'
import { Copy, FolderOpen, Inbox, Trash2, Wifi } from 'lucide-react'
import { programsApi, uploadLinksApi, type UploadLink } from '../services/api'
import { navigate } from '../hooks/useRoute'
import { useResource } from '../hooks/useResource'
import { useSession } from '../stores/session'
import { formatBytes, formatDate, formatRelative } from '../lib/format'
import { copyText } from './Pairing'
import { toastError } from './Toasts'
import { Badge, Button, ErrorNote, Field, IconButton, Input, Modal, Spinner } from './ui'

const selectClass = 'h-9 w-full rounded-md border border-line bg-bg px-2 text-[13px] text-fg outline-none focus:border-accent'

/**
 * Where a link can be opened from. Links live on the address for programs,
 * which must be on (and open to the local network for other devices).
 */
function Reach({ link, onChanged }: { link: UploadLink; onChanged: () => void }) {
  const { phase } = useSession()
  const manager = phase.kind === 'ready' && (phase.me.role === 'owner' || phase.me.role === 'administrator')
  const [busy, setBusy] = useState(false)
  if (link.reachable_from_network) return null
  const open = async () => {
    setBusy(true)
    try {
      const { settings } = await programsApi.get()
      await programsApi.set({ ...settings, enabled: true, lan: true })
      onChanged()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  return (
    <div className="mt-2 rounded-md border border-warn/40 bg-warn-soft px-3 py-2 text-[12.5px]">
      {link.urls.length === 0
        ? 'Upload links open on the address for programs, which is off: nobody can open this link yet.'
        : 'The link only opens on this computer: the address for programs is not open to the local network.'}
      {manager ? (
        <Button size="sm" className="mt-2" icon={<Wifi className="size-3.5" />} loading={busy} onClick={() => void open()}>
          Open it to the local network
        </Button>
      ) : (
        <span> Ask an owner or administrator to turn it on in Settings.</span>
      )}
    </div>
  )
}

function LinkBox({ link, onChanged }: { link: UploadLink; onChanged: () => void }) {
  const url = link.urls[0]
  return (
    <div>
      {url && (
        <div className="flex gap-2">
          <Input readOnly value={url} className="font-mono text-[12px]" onFocus={(e) => e.target.select()} />
          <IconButton label="Copy the link" onClick={() => void copyText(url, 'Link copied')}><Copy className="size-4" /></IconButton>
        </div>
      )}
      <Reach link={link} onChanged={onChanged} />
    </div>
  )
}

/** Makes an upload link for a folder, then shows it. */
export function AskForFilesDialog({ root_id, path, name, onClose }: { root_id: string; path: string; name: string; onClose: () => void }) {
  const [title, setTitle] = useState(name)
  const [message, setMessage] = useState('')
  const [days, setDays] = useState(7)
  const [limit, setLimit] = useState(2048)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [made, setMade] = useState<UploadLink | null>(null)

  const create = async () => {
    setBusy(true)
    setError('')
    try {
      setMade(await uploadLinksApi.create({ root_id, path, title, message, expires_days: days, max_mb: limit }))
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  const refresh = async () => {
    if (!made) return
    const all = await uploadLinksApi.list().catch(() => [])
    setMade(all.find((l) => l.id === made.id) ?? made)
  }

  return (
    <Modal
      open
      onClose={onClose}
      title={`Ask for files: ${name}`}
      footer={made ? <Button variant="primary" onClick={onClose}>Done</Button> : (
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!title.trim()} onClick={() => void create()}>Make the link</Button>
        </>
      )}
    >
      {made ? (
        <div className="space-y-3 text-[13px]">
          <p>Anyone with this link can add files to {name}. They see nothing that is there, and nothing is replaced.</p>
          <LinkBox link={made} onChanged={() => void refresh()} />
          <p className="text-[12px] text-subtle">It works for people on your local network (on your Wi-Fi, say), not over the internet. Take it down on Sharing.</p>
        </div>
      ) : (
        <div className="space-y-4">
          <p className="text-[12.5px] text-muted">
            A page people without HearthDrive can add files to this folder from, on your local network. Files are added as you, so it never
            does more than you may.
          </p>
          <Field label="Title" hint="Shown at the top of the page.">
            <Input autoFocus value={title} onChange={(e) => setTitle(e.target.value)} maxLength={100} />
          </Field>
          <Field label="Message (optional)">
            <textarea
              className="min-h-[70px] w-full rounded-md border border-line bg-bg px-2.5 py-2 text-[13px] outline-none focus:border-accent"
              value={message}
              maxLength={1000}
              onChange={(e) => setMessage(e.target.value)}
              placeholder="Please add your photos from the party."
            />
          </Field>
          <div className="grid grid-cols-2 gap-3">
            <Field label="Works for">
              <select className={selectClass} value={days} onChange={(e) => setDays(Number(e.target.value))}>
                <option value={1}>1 day</option>
                <option value={7}>7 days</option>
                <option value={30}>30 days</option>
                <option value={0}>Until taken down</option>
              </select>
            </Field>
            <Field label="Takes at most">
              <select className={selectClass} value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
                <option value={100}>100 MB</option>
                <option value={1024}>1 GB</option>
                <option value={2048}>2 GB</option>
                <option value={10240}>10 GB</option>
                <option value={51200}>50 GB</option>
              </select>
            </Field>
          </div>
          {error && <ErrorNote>{error}</ErrorNote>}
        </div>
      )}
    </Modal>
  )
}

/** The upload links someone made (everyone's, for owners and administrators). */
export function UploadLinksSection() {
  const links = useResource(() => uploadLinksApi.list(), [])
  const list = links.data ?? []
  const remove = async (l: UploadLink) => {
    if (!confirm(`Take down “${l.title}”? The link stops working at once; files already added stay.`)) return
    try {
      await uploadLinksApi.remove(l.id)
      links.reload()
    } catch (e) {
      toastError(e)
    }
  }
  return (
    <section>
      <h2 className="mb-3 text-[11px] font-semibold uppercase tracking-wider text-subtle">Upload links</h2>
      {links.error ? <ErrorNote>{links.error.message}</ErrorNote> : !links.data ? <Spinner /> : list.length === 0 ? (
        <p className="px-1 text-[12.5px] text-subtle">
          To collect files from people without HearthDrive, select a folder in Files and choose Ask for files. They add files from a page
          on your local network.
        </p>
      ) : (
        <ul className="divide-y divide-line rounded-lg border border-line bg-panel">
          {list.map((l) => (
            <li key={l.id} className="px-4 py-3 md:px-5">
              <div className="flex items-center gap-3">
                <Inbox className="size-4 shrink-0 text-accent" />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate font-medium">{l.title}</span>
                    {l.expired && <Badge tone="warn">{l.expires_at && l.expires_at <= Date.now() ? 'Expired' : 'Full'}</Badge>}
                  </div>
                  <div className="mt-0.5 truncate text-[12px] text-subtle">
                    Into {l.root_name}{l.path ? `/${l.path}` : ''} · {l.received_files} {l.received_files === 1 ? 'file' : 'files'},{' '}
                    {formatBytes(l.received_bytes)} of {formatBytes(l.max_bytes)} · by {l.created_by} {formatRelative(l.created_at)}
                    {l.expires_at ? ` · until ${formatDate(l.expires_at)}` : ''}
                  </div>
                </div>
                {l.urls[0] && !l.expired && (
                  <IconButton label="Copy the link" onClick={() => void copyText(l.urls[0], 'Link copied')}><Copy className="size-4" /></IconButton>
                )}
                <IconButton label="Open the folder" onClick={() => navigate('files', { root: l.root_id, path: l.path })}>
                  <FolderOpen className="size-4" />
                </IconButton>
                <IconButton label="Take down" onClick={() => void remove(l)}><Trash2 className="size-4" /></IconButton>
              </div>
              {!l.expired && <div className="pl-7"><Reach link={l} onChanged={links.reload} /></div>}
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}
