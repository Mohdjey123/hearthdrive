import { useCallback, useEffect, useState } from 'react'
import { Copy, Link2, Lock, Settings2, Share2, ShieldAlert, ShieldCheck, Trash2 } from 'lucide-react'
import { api, fileApi, isMobile, peerPrefix, thisDevice, type ShareLink } from '../services/api'
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
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.'))
  const peers = (devices.data ?? []).filter((d) => !d.is_local && d.online)
  const sources: Source[] = [
    ...(isMobile() ? [] : [{ id: '', name: `This ${thisDevice()}` }]),
    ...peers.map((d) => ({ id: d.id, name: d.name })),
  ]
  return (
    <div>
      <PageHeader
        title="Shares"
        subtitle="Links anyone can download your files with, until they expire"
        actions={!isMobile() && <ServiceSettings />}
      />
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
