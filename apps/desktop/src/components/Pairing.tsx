import { useCallback, useEffect, useState } from 'react'
import QRCode from 'qrcode'
import { Check, Copy, Fingerprint, Laptop, Link2, RefreshCw, ShieldAlert } from 'lucide-react'
import { api, ApiError, type Invite, type JoinState, type PairingRequest } from '../services/api'
import { subscribe } from '../services/events'
import { formatRelative } from '../lib/format'
import { toast, toastError } from './Toasts'
import { Badge, Button, ErrorNote, Modal, Spinner, cx } from './ui'

function hostOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/\.$/, '')
  } catch {
    return url
  }
}

/** Splits a route (`direct <ip:port>` / `relay <url>`) into a label and a place. */
export function routeParts(route: string): { kind: 'Direct' | 'Relayed'; where: string } {
  const [kind, ...rest] = route.split(' ')
  const where = rest.join(' ')
  return kind === 'relay' ? { kind: 'Relayed', where: hostOf(where) } : { kind: 'Direct', where }
}

export function describeRoute(route: string): string {
  const r = routeParts(route)
  return r.kind === 'Direct' ? `directly (${r.where})` : `through relay ${r.where}`
}

/** A readable form of an address hint (`relay:<url>` or `ip:port`). */
export function describeHint(hint: string): string {
  return hint.startsWith('relay:') ? `relay ${hostOf(hint.slice(6))}` : hint
}

export async function copyText(text: string, what = 'Copied') {
  try {
    await navigator.clipboard.writeText(text)
    toast(what)
  } catch {
    toast('Copy failed — select the text and copy it manually', 'error')
  }
}

export function FingerprintText({ value, className }: { value: string | null | undefined; className?: string }) {
  if (!value) return <span className="text-subtle">—</span>
  return <span className={cx('font-mono text-[12px] tracking-wide break-all', className)}>{value}</span>
}

function useCountdown(until: number | null) {
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    if (!until) return
    const t = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(t)
  }, [until])
  if (!until) return null
  const left = Math.max(0, Math.round((until - now) / 1000))
  return { left, text: `${Math.floor(left / 60)}:${String(left % 60).padStart(2, '0')}` }
}

/** A pending request: who is asking, and the fingerprint to compare. */
export function RequestCard({ request, onDecided }: { request: PairingRequest; onDecided?: (approved: boolean) => void }) {
  const [busy, setBusy] = useState<'approve' | 'reject' | null>(null)
  const decide = async (approve: boolean) => {
    setBusy(approve ? 'approve' : 'reject')
    try {
      if (approve) {
        await api.approvePairing(request.id)
        toast(`${request.name} was added to your cloud`)
      } else {
        await api.rejectPairing(request.id)
        toast(`Declined ${request.name}`)
      }
      onDecided?.(approve)
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(null)
    }
  }
  return (
    <div className="rounded-lg border border-accent/40 bg-accent-soft p-4">
      <div className="flex items-start gap-3">
        <Laptop className="mt-0.5 size-5 shrink-0 text-accent" />
        <div className="min-w-0 flex-1">
          <div className="font-medium">{request.name} wants to join</div>
          <div className="mt-0.5 text-[12px] text-muted">
            <span className="capitalize">{request.device_type}</span>
            {formatRelative(request.created_at)}
          </div>
          <div className="mt-3 text-[12px] text-muted">Device fingerprint — check it matches the one shown on {request.name}:</div>
          <FingerprintText value={request.fingerprint} className="mt-1 block text-fg" />
        </div>
      </div>
      <div className="mt-4 flex justify-end gap-2">
        <Button size="sm" variant="ghost" loading={busy === 'reject'} disabled={!!busy} onClick={() => void decide(false)}>Decline</Button>
        <Button size="sm" variant="primary" loading={busy === 'approve'} disabled={!!busy} onClick={() => void decide(true)}>
          Approve
        </Button>
      </div>
    </div>
  )
}

/** Shows an invitation as a QR code and a link, then the request it receives. */
export function InviteModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [invite, setInvite] = useState<Invite | null>(null)
  const [qr, setQr] = useState<string | null>(null)
  const [error, setError] = useState<ApiError | null>(null)
  const [requests, setRequests] = useState<PairingRequest[]>([])
  const [paired, setPaired] = useState(false)
  const countdown = useCountdown(invite?.expires_at ?? null)
  const expired = countdown?.left === 0

  const create = useCallback(async () => {
    setError(null)
    setInvite(null)
    setQr(null)
    setRequests([])
    setPaired(false)
    try {
      const inv = await api.createInvite()
      setInvite(inv)
      setQr(await QRCode.toDataURL(inv.link, { margin: 1, width: 232, errorCorrectionLevel: 'M', color: { dark: '#000000', light: '#ffffff' } }))
    } catch (e) {
      setError(e as ApiError)
    }
  }, [])

  useEffect(() => {
    if (open) void create()
  }, [open, create])

  // Show requests that arrive for this invitation.
  useEffect(() => {
    if (!invite) return
    const load = () =>
      api.pairingRequests().then((all) => setRequests(all.filter((r) => r.session_id === invite.session_id))).catch(() => {})
    load()
    const off = subscribe((e) => {
      if (e.type === 'pairing.requested' || e.type === 'device.changed') load()
    })
    const poll = setInterval(load, 4000)
    return () => {
      off()
      clearInterval(poll)
    }
  }, [invite])

  const close = useCallback(() => {
    if (invite && !paired) void api.cancelInvite(invite.session_id).catch(() => {})
    setInvite(null)
    onClose()
  }, [invite, paired, onClose])

  return (
    <Modal
      open={open}
      onClose={close}
      title="Add a device"
      footer={<Button variant={paired ? 'primary' : 'ghost'} onClick={close}>{paired ? 'Done' : 'Close'}</Button>}
    >
      {error ? (
        <div className="space-y-3">
          <ErrorNote>{error.message}</ErrorNote>
          {error.code === 'conflict' && (
            <p className="text-muted">See <span className="font-medium text-fg">Private network</span> on the Devices page for setup steps.</p>
          )}
          <Button size="sm" icon={<RefreshCw className="size-3.5" />} onClick={() => void create()}>Try again</Button>
        </div>
      ) : !invite ? (
        <div className="flex justify-center py-10"><Spinner /></div>
      ) : paired ? (
        <div className="flex flex-col items-center gap-2 py-8 text-center">
          <div className="rounded-full bg-accent-soft p-3"><Check className="size-6 text-accent" /></div>
          <div className="font-medium">Device added</div>
          <p className="text-muted">It can now browse this computer's files, and you can browse its files from here.</p>
        </div>
      ) : requests.length > 0 ? (
        <div className="space-y-3">
          {requests.map((r) => (
            <RequestCard key={r.id} request={r} onDecided={(ok) => (ok ? setPaired(true) : setRequests((rs) => rs.filter((x) => x.id !== r.id)))} />
          ))}
        </div>
      ) : (
        <div className="space-y-4">
          <ol className="list-decimal space-y-1 pl-5 text-muted">
            <li>On the other computer, open NEXUS Cloud → <span className="text-fg">Devices</span> → <span className="text-fg">Join a cloud</span>.</li>
            <li>Paste this link there. You will approve the request here.</li>
          </ol>
          <div className="flex gap-4">
            <div className={cx('shrink-0 rounded-md bg-white p-1.5', expired && 'opacity-30')}>
              {qr ? <img src={qr} alt="Pairing QR code" className="size-[132px]" /> : <div className="size-[132px]" />}
            </div>
            <div className="min-w-0 flex-1 space-y-2">
              <div className="text-[12px] text-subtle">Pairing link</div>
              <div className="max-h-[74px] overflow-hidden rounded-md border border-line bg-bg px-2 py-1.5 font-mono text-[11px] break-all text-muted select-all">
                {invite.link}
              </div>
              <Button size="sm" icon={<Copy className="size-3.5" />} disabled={expired} onClick={() => void copyText(invite.link, 'Pairing link copied')}>
                Copy link
              </Button>
            </div>
          </div>
          <div className="flex items-center justify-between rounded-md bg-elevated px-3 py-2 text-[12px]">
            {expired ? (
              <span className="text-warn">This code expired.</span>
            ) : (
              <span className="text-muted">Single use · expires in <span className="text-fg tabular">{countdown?.text}</span></span>
            )}
            {expired ? (
              <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} onClick={() => void create()}>New code</Button>
            ) : (
              <span className="flex items-center gap-1.5 text-muted"><Spinner className="size-3" /> Waiting for the device</span>
            )}
          </div>
          <div className="text-[12px] text-muted">
            <div className="flex items-center gap-1.5"><Fingerprint className="size-3.5" /> This computer's fingerprint</div>
            <FingerprintText value={invite.fingerprint} className="mt-1 block text-fg" />
          </div>
          <p className="text-[12px] text-subtle">
            The link contains this computer's public key, how to reach it and a one-time code — never a password or private key.
            Phones will scan the QR code once Android access arrives.
          </p>
        </div>
      )}
    </Modal>
  )
}

/** Paste a link from another computer and wait for its owner to approve. */
export function JoinForm({ onJoined, onCancel }: { onJoined: (join: JoinState) => void; onCancel?: () => void }) {
  const [link, setLink] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [join, setJoin] = useState<JoinState | null>(null)

  useEffect(() => {
    if (!join || join.status !== 'waiting') return
    const refresh = () =>
      api.joinStatus(join.id).then((j) => setJoin(j)).catch(() => {})
    const off = subscribe((e) => {
      if (e.type === 'pairing.updated' && e.id === join.id) refresh()
    })
    const poll = setInterval(refresh, 2500)
    return () => {
      off()
      clearInterval(poll)
    }
  }, [join])

  useEffect(() => {
    if (join?.status === 'approved') onJoined(join)
  }, [join?.status])

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setError('')
    try {
      setJoin(await api.join(link.trim()))
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  if (join) {
    const failed = join.status !== 'waiting' && join.status !== 'approved'
    return (
      <div className="space-y-4">
        {join.status === 'approved' ? (
          <div className="flex items-center gap-2 font-medium"><Check className="size-4 text-accent" /> Joined {join.host_name}</div>
        ) : failed ? (
          <>
            <div className="flex items-center gap-2 font-medium"><ShieldAlert className="size-4 text-danger" /> Not paired</div>
            <ErrorNote>{join.error ?? `The request is ${join.status}.`}</ErrorNote>
          </>
        ) : (
          <div className="flex items-center gap-2 font-medium">
            <Spinner className="size-4" /> Waiting for approval on {join.host_name}
          </div>
        )}
        <div className="rounded-md bg-elevated px-3 py-2.5">
          <div className="text-[12px] text-muted">
            Fingerprint of {join.host_name} — it must match the one shown there:
          </div>
          <FingerprintText value={join.host_fingerprint} className="mt-1 block text-fg" />
          {join.route && <div className="mt-2 text-[12px] text-subtle">Connected {describeRoute(join.route)}</div>}
        </div>
        <div className="flex justify-end gap-2">
          {failed && <Button variant="ghost" onClick={() => setJoin(null)}>Try again</Button>}
          {onCancel && join.status === 'waiting' && <Button variant="ghost" onClick={onCancel}>Close</Button>}
        </div>
      </div>
    )
  }

  return (
    <form onSubmit={submit} className="space-y-3">
      <p className="text-muted">
        On the computer you are joining, open <span className="text-fg">Devices</span> → <span className="text-fg">Add device</span> and copy its pairing link.
        Both computers need NEXUS running and an internet connection, or the same local network. Nothing else to install.
      </p>
      <label className="block">
        <span className="mb-1 block text-[12px] text-muted">Pairing link</span>
        <textarea
          value={link}
          onChange={(e) => setLink(e.target.value)}
          rows={4}
          spellCheck={false}
          placeholder="nexus-pair:…"
          className="w-full resize-none rounded-md border border-line bg-bg px-3 py-2 font-mono text-[11.5px] text-fg outline-none placeholder:text-subtle focus:border-accent"
          autoFocus
        />
      </label>
      <ErrorNote>{error}</ErrorNote>
      <div className="flex justify-end gap-2">
        {onCancel && <Button type="button" variant="ghost" onClick={onCancel}>Cancel</Button>}
        <Button type="submit" variant="primary" icon={<Link2 className="size-3.5" />} loading={busy} disabled={!link.trim()}>
          Join
        </Button>
      </div>
    </form>
  )
}

export function JoinModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [done, setDone] = useState<JoinState | null>(null)
  useEffect(() => {
    if (open) setDone(null)
  }, [open])
  return (
    <Modal open={open} onClose={onClose} title="Join a cloud" footer={done && <Button variant="primary" onClick={onClose}>Done</Button>}>
      {done ? (
        <div className="flex flex-col items-center gap-2 py-6 text-center">
          <div className="rounded-full bg-accent-soft p-3"><Check className="size-6 text-accent" /></div>
          <div className="font-medium">Paired with {done.host_name}</div>
          <p className="text-muted">You can browse its files from Files → device menu.</p>
          <Badge tone="accent">Each computer keeps its own files</Badge>
        </div>
      ) : (
        <JoinForm onJoined={setDone} onCancel={onClose} />
      )}
    </Modal>
  )
}
