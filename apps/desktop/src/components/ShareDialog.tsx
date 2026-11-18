import { useEffect, useState } from 'react'
import { Check, Copy, Lock, ShieldAlert, ShieldCheck, Share2 } from 'lucide-react'
import { fileApi, type FileEntry, type ShareLink } from '../services/api'
import { useResource } from '../hooks/useResource'
import { navigate } from '../hooks/useRoute'
import { formatBytes } from '../lib/format'
import { copyText } from './Pairing'
import { Button, ErrorNote, Field, Input, Modal, ProgressBar, Spinner, cx } from './ui'

const EXPIRY: [number, string][] = [
  [300, '5 minutes'],
  [3600, '1 hour'],
  [86400, '1 day'],
  [3 * 86400, '3 days'],
  [7 * 86400, '7 days'],
]
const DOWNLOADS = [1, 2, 3, 5, 10, 20, 50, 100]

export function expiryLabel(secs: number) {
  return EXPIRY.find(([s]) => s === secs)?.[1] ?? `${Math.round(secs / 3600)} hours`
}

const select = 'h-8 w-full rounded-md border border-line bg-bg px-2 text-fg outline-none focus:border-accent'

/** Hands a link to the phone's share sheet, or copies it. */
export async function passOn(link: ShareLink) {
  if (!link.url) return
  if (typeof navigator.share === 'function' && /Android|iPhone|iPad/i.test(navigator.userAgent)) {
    try {
      await navigator.share({ title: link.name, url: link.url })
      return
    } catch (e) {
      if ((e as Error).name === 'AbortError') return
    }
  }
  await copyText(link.url, 'Link copied')
}

/**
 * Makes a link anyone can download a file or folder with. Send encrypts it
 * on the device holding it first; temp.sh takes it as it is.
 */
export function ShareDialog({ file, prefix, onClose }: { file: FileEntry; prefix: string; onClose: () => void }) {
  const fapi = fileApi(prefix)
  const services = useResource(() => fapi.shareServices(), [prefix])
  const [provider, setProvider] = useState<'send' | 'tempsh'>('send')
  const [expires, setExpires] = useState(86400)
  const [downloads, setDownloads] = useState(1)
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [link, setLink] = useState<ShareLink | null>(null)

  const limits = services.data?.send
  const expiryChoices = EXPIRY.filter(([s]) => !limits || s <= limits.MAX_EXPIRE_SECONDS)
  const downloadChoices = DOWNLOADS.filter((n) => !limits || n <= limits.MAX_DOWNLOADS)
  const sendDown = !!services.data && !limits
  useEffect(() => {
    if (sendDown) setProvider('tempsh')
  }, [sendDown])
  useEffect(() => {
    if (expiryChoices.length && !expiryChoices.some(([s]) => s === expires)) setExpires(expiryChoices[expiryChoices.length - 1][0])
  }, [limits?.MAX_EXPIRE_SECONDS])

  // Follow the upload until the link works.
  const uploading = link?.status === 'uploading'
  useEffect(() => {
    if (!uploading || !link) return
    const t = setInterval(() => {
      fapi.shares()
        .then((all) => {
          const now = all.find((s) => s.id === link.id)
          if (now) setLink(now)
        })
        .catch(() => {})
    }, 1000)
    return () => clearInterval(t)
  }, [uploading, link?.id])

  const tooBig =
    !file.is_dir &&
    (provider === 'send' ? !!limits && file.size > limits.MAX_FILE_SIZE : !!services.data && file.size > services.data.tempsh_max_size)

  const create = async () => {
    setBusy(true)
    setError('')
    try {
      const made = await fapi.createShare({
        file_id: file.id,
        provider,
        ...(provider === 'send' ? { expires_secs: expires, downloads, password: password || undefined } : {}),
      })
      setLink(made)
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  const name = file.is_dir ? `${file.filename}.zip` : file.filename

  if (link) {
    const pct = link.bytes_total ? (link.bytes_done ?? 0) / link.bytes_total : 0
    return (
      <Modal
        open
        onClose={onClose}
        title={`Share ${file.filename}`}
        footer={
          <>
            <Button variant="ghost" onClick={() => { onClose(); navigate('shares') }}>All links</Button>
            <Button variant="primary" onClick={onClose}>Done</Button>
          </>
        }
      >
        {link.status === 'uploading' ? (
          <div className="space-y-2">
            <p>
              {link.provider === 'send' ? 'Encrypting and uploading' : 'Uploading'} <span className="font-medium">{name}</span>…
            </p>
            <ProgressBar value={pct} />
            <p className="text-[12px] text-subtle">
              {link.bytes_total ? `${formatBytes(link.bytes_done ?? 0)} of ${formatBytes(link.bytes_total)}` : file.is_dir ? 'Packing the folder' : 'Starting'}
              {' · '}You can close this; the upload continues and the link appears under Shares.
            </p>
          </div>
        ) : link.status === 'active' && link.url ? (
          <div className="space-y-3">
            <p className="flex items-center gap-1.5 font-medium"><Check className="size-4 text-accent" /> The link is ready</p>
            <div className="flex gap-2">
              <Input readOnly value={link.url} onFocus={(e) => e.target.select()} className="font-mono text-[12px]" />
              <Button variant="primary" icon={<Copy className="size-3.5" />} onClick={() => void copyText(link.url!, 'Link copied')}>Copy</Button>
            </div>
            {typeof navigator.share === 'function' && (
              <Button icon={<Share2 className="size-3.5" />} onClick={() => void passOn(link)}>Send with another app</Button>
            )}
            <p className="text-[12px] text-subtle">
              {link.provider === 'send'
                ? `Works for ${link.downloads === 1 ? '1 download' : `${link.downloads} downloads`} or ${expiryLabel(expires)}, whichever comes first.${link.has_password ? ' People need the password too: send it separately.' : ''} Anyone with the link can download the file.`
                : 'Works for 3 days. Anyone with the link can download the file.'}
            </p>
          </div>
        ) : (
          <ErrorNote>{link.error ?? 'The upload did not finish'}</ErrorNote>
        )}
      </Modal>
    )
  }

  return (
    <Modal
      open
      onClose={busy ? () => {} : onClose}
      title={`Share ${file.filename}`}
      footer={
        <>
          <Button variant="ghost" disabled={busy} onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!services.data || tooBig} onClick={() => void create()}>
            Create link
          </Button>
        </>
      }
    >
      {!services.data ? (
        services.error ? <ErrorNote>{services.error.message}</ErrorNote> : <Spinner />
      ) : (
        <div className="space-y-4">
          <div className="grid gap-2 sm:grid-cols-2">
            <ProviderCard
              active={provider === 'send'}
              disabled={sendDown}
              onClick={() => setProvider('send')}
              icon={<ShieldCheck className="size-4 text-accent" />}
              title="Send (encrypted)"
              text={
                sendDown
                  ? services.data.send_error ?? 'Not reachable right now'
                  : `Encrypted before it leaves this device. Up to ${formatBytes(limits!.MAX_FILE_SIZE)}.`
              }
            />
            <ProviderCard
              active={provider === 'tempsh'}
              onClick={() => setProvider('tempsh')}
              icon={<ShieldAlert className="size-4 text-warn" />}
              title="temp.sh"
              text="Not encrypted: the service can read it. Up to 4 GB, 3 days."
            />
          </div>

          {provider === 'send' ? (
            <>
              <div className="grid grid-cols-2 gap-3">
                <Field label="Link works for">
                  <select value={expires} onChange={(e) => setExpires(Number(e.target.value))} className={select}>
                    {expiryChoices.map(([s, label]) => <option key={s} value={s}>{label}</option>)}
                  </select>
                </Field>
                <Field label="Downloads">
                  <select value={downloads} onChange={(e) => setDownloads(Number(e.target.value))} className={select}>
                    {downloadChoices.map((n) => <option key={n} value={n}>{n === 1 ? '1 download' : `${n} downloads`}</option>)}
                  </select>
                </Field>
              </div>
              <Field label="Password (optional)" hint="People need it to download. It is never stored on this device.">
                <div className="relative">
                  <Lock className="pointer-events-none absolute top-2 left-2.5 size-4 text-subtle" />
                  <Input type="password" autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} className="pl-8" />
                </div>
              </Field>
              <p className="text-[12px] text-subtle">
                The key is in the link itself, after the #, so {hostOf(services.data.send_url)} stores only data it cannot read.
                Anyone you give the link to can download {file.is_dir ? 'the folder' : 'the file'}.
              </p>
            </>
          ) : (
            <p className="rounded-md bg-warn-soft p-2.5 text-[12.5px]">
              temp.sh receives {file.is_dir ? 'the folder' : 'the file'} as it is. Use it only for things that are not private.
              The link works for 3 days and cannot be taken down early.
            </p>
          )}
          {file.is_dir && <p className="text-[12px] text-subtle">The folder is sent as one file, {name}.</p>}
          {tooBig && <ErrorNote>This is larger than the service accepts.</ErrorNote>}
          {error && <ErrorNote>{error}</ErrorNote>}
        </div>
      )}
    </Modal>
  )
}

function ProviderCard({ active, disabled, onClick, icon, title, text }: {
  active: boolean
  disabled?: boolean
  onClick: () => void
  icon: React.ReactNode
  title: string
  text: string
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={cx(
        'rounded-md border p-2.5 text-left transition-colors disabled:opacity-50',
        active ? 'border-accent bg-accent-soft' : 'border-line hover:bg-hover',
      )}
    >
      <div className="flex items-center gap-1.5 font-medium">{icon} {title}</div>
      <div className="mt-1 text-[12px] text-muted">{text}</div>
    </button>
  )
}

export function hostOf(url: string) {
  try { return new URL(url).host } catch { return url }
}
