import { useCallback, useEffect, useState } from 'react'
import { Copy, FolderOpen, Link2, Lock, Monitor, Pencil, Settings2, Share2, ShieldAlert, ShieldCheck, Trash2, User, Users } from 'lucide-react'
import { api, fileApi, isMobile, peerPrefix, thisDevice, type SharedFolder, type ShareLink } from '../services/api'
import { navigate } from '../hooks/useRoute'
import { useSession } from '../stores/session'
import { AccessBadge, ACCESS, ShareFolderDialog } from '../components/FolderSharing'
import { GroupsSection } from '../components/Groups'
import { UploadLinksSection } from '../components/UploadLinks'
import { useResource } from '../hooks/useResource'
import { formatBytes, formatRelative } from '../lib/format'
import { copyText } from '../components/Pairing'
import { hostOf, passOn } from '../components/ShareDialog'
import { toast, toastError } from '../components/Toasts'
import { Badge, Button, EmptyState, ErrorNote, Field, IconButton, Input, Modal, PageHeader, ProgressBar, Spinner } from '../components/ui'

/** A device whose links are listed: this one (`''`) or a paired computer. */
interface Source {
  id: string
  name: string
}

function StatusBadge({ link }: { link: ShareLink }) {
  switch (link.status) {
    case 'uploading': return <Badge tone="accent">Uploading</Badge>
    case 'active': return <Badge tone="accent">Active</Badge>
    case 'expired': return <Badge>Expired</Badge>
    case 'failed': return <Badge tone="danger">Failed</Badge>
  }
}

function describe(link: ShareLink) {
  const parts: string[] = []
  if (link.status === 'active') parts.push(`expires ${formatRelative(link.expires_at)}`)
  if (link.provider === 'send' && link.downloads != null && link.status !== 'failed') {
    parts.push(`${link.downloaded} of ${link.downloads} download${link.downloads === 1 ? '' : 's'}`)
  }
  if (link.size) parts.push(formatBytes(link.size))
  parts.push(link.provider === 'send' ? `Send · ${hostOf(link.service)}` : 'temp.sh, not encrypted')
  parts.push(`by ${link.created_by} ${formatRelative(link.created_at)}`)
  return parts.join(' · ')
}

function LinkRow({ link, onRemove }: { link: ShareLink; onRemove: () => void }) {
  const pct = link.bytes_total ? (link.bytes_done ?? 0) / link.bytes_total : 0
  const Icon = link.provider === 'send' ? ShieldCheck : ShieldAlert
  return (
    <li className="px-4 py-3 md:px-5">
      <div className="flex items-center gap-3">
        <Icon className={link.provider === 'send' ? 'size-4 shrink-0 text-accent' : 'size-4 shrink-0 text-warn'} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate font-medium">{link.name}</span>
            <StatusBadge link={link} />
            {link.has_password && <Lock className="size-3.5 shrink-0 text-subtle" aria-label="Password" />}
          </div>
          <div className="mt-0.5 text-[12px] text-subtle">{describe(link)}</div>
          {link.status === 'uploading' && <ProgressBar className="mt-2" value={pct} />}
          {link.status === 'failed' && link.error && <div className="mt-1 text-[12px] text-danger">{link.error}</div>}
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {link.status === 'active' && link.url && (
            <>
              {isMobile() && typeof navigator.share === 'function' ? (
                <IconButton label="Share link" onClick={() => void passOn(link)}><Share2 className="size-4" /></IconButton>
              ) : null}
              <Button size="sm" icon={<Copy className="size-3.5" />} onClick={() => void copyText(link.url!, 'Link copied')}>
                <span className="hidden sm:inline">Copy link</span>
              </Button>
            </>
          )}
          <IconButton label={link.status === 'active' ? 'Take down' : 'Remove'} className="hover:text-danger" onClick={onRemove}>
            <Trash2 className="size-4" />
          </IconButton>
        </div>
      </div>
    </li>
  )
}

/** The links one device made, checked with the services when shown. */
function SourceLinks({ source, showName }: { source: Source; showName: boolean }) {
  const fapi = fileApi(source.id ? peerPrefix(source.id) : '')
  const [links, setLinks] = useState<ShareLink[] | null>(null)
  const [error, setError] = useState<Error | null>(null)
  const [removing, setRemoving] = useState<ShareLink | null>(null)
  const [busy, setBusy] = useState(false)

  const load = useCallback(
    (check = false) =>
      (check ? fapi.refreshShares() : fapi.shares())
        .then((l) => { setLinks(l); setError(null) })
        .catch((e) => setError(e as Error)),
    [fapi],
  )
  useEffect(() => { void load(true) }, [load])
  const uploading = links?.some((l) => l.status === 'uploading') ?? false
  useEffect(() => {
    if (!uploading) return
    const t = setInterval(() => void load(), 1500)
    return () => clearInterval(t)
  }, [uploading, load])

  const remove = async () => {
    if (!removing) return
    setBusy(true)
    try {
      await fapi.deleteShare(removing.id)
      toast(removing.status === 'active' && removing.provider === 'send' ? `Took down ${removing.name}` : `Removed ${removing.name}`)
      setRemoving(null)
      void load()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }

  if (!links && !error) return <div className="p-4"><Spinner /></div>
  return (
    <section>
      {showName && <h2 className="px-1 pb-2 text-[12px] font-semibold tracking-wide text-muted uppercase">{source.name}</h2>}
      {error ? (
        <ErrorNote>{error.message}</ErrorNote>
      ) : links!.length === 0 ? (
        showName ? (
          <p className="px-1 text-[12.5px] text-subtle">No links.</p>
        ) : (
          <EmptyState icon={<Link2 className="size-8" />} title="No links yet" />
        )
      ) : (
        <ul className="divide-y divide-line rounded-lg border border-line bg-panel">
          {links!.map((l) => <LinkRow key={l.id} link={l} onRemove={() => setRemoving(l)} />)}
        </ul>
      )}
      <Modal
        open={!!removing}
        onClose={() => setRemoving(null)}
        title={removing?.status === 'active' ? 'Take down this link?' : 'Remove this link?'}
        footer={
          <>
            <Button variant="ghost" onClick={() => setRemoving(null)}>Cancel</Button>
            <Button variant="danger" loading={busy} onClick={() => void remove()}>
              {removing?.status === 'active' ? 'Take down' : 'Remove'}
            </Button>
          </>
        }
      >
        {removing?.status === 'active' && removing.provider === 'send' ? (
          <p>The file is deleted from {hostOf(removing.service)} and <span className="font-medium">{removing.name}</span> can no longer be downloaded. Your own copy stays where it is.</p>
        ) : removing?.status === 'active' ? (
          <p>temp.sh cannot delete files early: the link keeps working until it expires. It is only removed from this list.</p>
        ) : removing?.status === 'uploading' ? (
          <p>The upload is stopped.</p>
        ) : (
          <p><span className="font-medium">{removing?.name}</span> is removed from the list.</p>
        )}
      </Modal>
    </section>
  )
}

const heading = 'px-1 pb-2 text-[12px] font-semibold tracking-wide text-muted uppercase'

const where = (f: { root: { name: string }; path: string }) => (f.path ? `${f.root.name} / ${f.path}` : f.root.name)

/** Folders others shared with the person signed in. */
function SharedWithYou() {
  const mine = useResource(() => api.sharedWithMe(), [])
  const list = mine.data ?? []
  if (list.length === 0) return null
  return (
    <section>
      <h2 className={heading}>Shared with you</h2>
      <ul className="divide-y divide-line rounded-lg border border-line bg-panel">
        {list.map((s) => (
          <li key={s.id} className="flex items-center gap-3 px-4 py-3 md:px-5">
            <Users className="size-4 shrink-0 text-accent" />
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <span className="truncate font-medium">{s.name}</span>
                <AccessBadge access={s.access} />
              </div>
              <div className="mt-0.5 text-[12px] text-subtle">Shared by {s.shared_by} · {ACCESS[s.access].hint}</div>
            </div>
            <Button size="sm" icon={<FolderOpen className="size-3.5" />} onClick={() => navigate('files', { root: s.root.id, path: s.path })}>
              Open
            </Button>
          </li>
        ))}
      </ul>
    </section>
  )
}

/** Every shared folder on this computer, for owners and administrators. */
function SharedFolders({ version }: { version: number }) {
  const folders = useResource(() => api.sharedFolders(), [version])
  const [editing, setEditing] = useState<SharedFolder | null>(null)
  const list = folders.data ?? []
  return (
    <section>
      <h2 className={heading}>Shared folders</h2>
      {folders.error ? (
        <ErrorNote>{folders.error.message}</ErrorNote>
      ) : !folders.data ? (
        <Spinner />
      ) : list.length === 0 ? (
        <p className="px-1 text-[12.5px] text-subtle">
          No folders are shared. To share one with people on this computer or with paired devices, select it in Files and choose
          Share with people.
        </p>
      ) : (
        <ul className="divide-y divide-line rounded-lg border border-line bg-panel">
          {list.map((f) => (
            <li key={f.id} className="px-4 py-3 md:px-5">
              <div className="flex items-center gap-3">
                <Users className="size-4 shrink-0 text-accent" />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate font-medium">{f.name}</span>
                    {f.path && !f.file_id && <Badge tone="warn">Folder not found</Badge>}
                  </div>
                  <div className="mt-0.5 truncate text-[12px] text-subtle">
                    {where(f)} · by {f.created_by} {formatRelative(f.created_at)}
                  </div>
                </div>
                <div className="flex shrink-0 items-center gap-1">
                  <IconButton label="Open in Files" onClick={() => navigate('files', { root: f.root.id, path: f.path })}>
                    <FolderOpen className="size-4" />
                  </IconButton>
                  <Button size="sm" icon={<Pencil className="size-3.5" />} onClick={() => setEditing(f)}>Edit</Button>
                </div>
              </div>
              {f.members.length === 0 ? (
                <p className="mt-2 pl-7 text-[12px] text-subtle">Not shared with anyone at the moment.</p>
              ) : (
                <ul className="mt-2 flex flex-wrap gap-1.5 pl-7">
                  {f.members.map((m) => (
                    <li
                      key={m.member}
                      title={m.note ?? ACCESS[m.access].hint}
                      className="flex items-center gap-1.5 rounded-md border border-line bg-elevated px-2 py-0.5 text-[12px]"
                    >
                      {m.kind === 'device' ? <Monitor className="size-3 text-subtle" />
                        : m.kind === 'group' ? <Users className="size-3 text-subtle" />
                        : <User className="size-3 text-subtle" />}
                      {m.name}
                      <span className={m.note && m.kind !== 'group' ? 'text-warn' : 'text-subtle'}>· {ACCESS[m.access].label.toLowerCase()}</span>
                    </li>
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>
      )}
      {editing && (
        <ShareFolderDialog
          target={{ root_id: editing.root.id, path: editing.path, name: editing.name }}
          onClose={() => setEditing(null)}
          onSaved={folders.reload}
        />
      )}
    </section>
  )
}

function ServiceSettings() {
  const services = useResource(() => api.shareServices(), [])
  const [open, setOpen] = useState(false)
  const [url, setUrl] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  if (!services.data?.can_configure) return null
  const save = async (value: string) => {
    setBusy(true)
    setError('')
    try {
      await api.setShareServices(value)
      services.reload()
      setOpen(false)
      toast('Send service saved')
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <>
      <Button size="sm" icon={<Settings2 className="size-3.5" />} onClick={() => { setUrl(services.data!.send_url); setOpen(true) }}>
        Send service
      </Button>
      <Modal
        open={open}
        onClose={() => setOpen(false)}
        title="Send service"
        footer={
          <>
            <Button variant="ghost" disabled={busy} onClick={() => void save('')}>Use the default</Button>
            <Button variant="primary" loading={busy} onClick={() => void save(url)}>Save</Button>
          </>
        }
      >
        <div className="space-y-3">
          <Field label="Address" hint="Any service running Send (github.com/timvisee/send), including one you host yourself.">
            <Input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://send.vis.ee" />
          </Field>
          <p className="text-[12px] text-subtle">Files are encrypted before they are uploaded, so the service never sees what they contain.</p>
          {error && <ErrorNote>{error}</ErrorNote>}
        </div>
      </Modal>
    </>
  )
}

export function Shares() {
  const { phase } = useSession()
  const manager = phase.kind === 'ready' && (phase.me.role === 'owner' || phase.me.role === 'administrator')
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.'))
  const peers = (devices.data ?? []).filter((d) => !d.is_local && d.online)
  const [version, setVersion] = useState(0)
  const sources: Source[] = [
    ...(isMobile() ? [] : [{ id: '', name: `This ${thisDevice()}` }]),
    ...peers.map((d) => ({ id: d.id, name: d.name })),
  ]
  return (
    <div>
      <PageHeader
        title="Sharing"
        subtitle="Folders shared with people and devices, and links anyone can download with until they expire"
        actions={!isMobile() && <ServiceSettings />}
      />
      {!isMobile() && (
        <div className="mb-8 space-y-8">
          <SharedWithYou />
          {manager && <SharedFolders version={version} />}
          {manager && <GroupsSection onChanged={() => setVersion((v) => v + 1)} />}
          <UploadLinksSection />
        </div>
      )}
      {!isMobile() && <h2 className={heading}>Links</h2>}
      {devices.loading && !devices.data ? (
        <Spinner />
      ) : sources.length === 0 ? (
        <EmptyState icon={<Link2 className="size-8" />} title="No computer to share from">
          Pair this phone with a computer that runs NEXUS, then share its files from Files.
        </EmptyState>
      ) : (
        <div className="space-y-6">
          {sources.map((s) => <SourceLinks key={s.id} source={s} showName={sources.length > 1} />)}
          <p className="text-[12px] text-subtle">
            To make a link, select a file or folder in Files and choose Share. Links made with Send are encrypted on the
            computer holding the file; the key is part of the link and never reaches the service.
          </p>
        </div>
      )}
    </div>
  )
}
