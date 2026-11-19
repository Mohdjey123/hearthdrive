import { useEffect, useState } from 'react'
import { Copy, KeyRound, Plus, Trash2 } from 'lucide-react'
import { programsApi, type CreatedToken, type ProgramSettings } from '../services/api'
import { useResource } from '../hooks/useResource'
import { formatDate, formatRelative } from '../lib/format'
import { copyText } from './Pairing'
import { toast, toastError } from './Toasts'
import { Badge, Button, ErrorNote, Field, IconButton, Input, Modal, Panel, Spinner } from './ui'

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-1 items-start gap-1 py-3 md:grid-cols-[200px_1fr] md:gap-4">
      <div className="text-muted">{label}</div>
      <div className="min-w-0 break-words">{children}</div>
    </div>
  )
}

function Secret({ label, value }: { label: string; value: string }) {
  return (
    <Field label={label}>
      <div className="flex gap-2">
        <Input readOnly value={value} className="font-mono text-[12px]" onFocus={(e) => e.target.select()} />
        <IconButton label={`Copy ${label.toLowerCase()}`} onClick={() => void copyText(value, `${label} copied`)}><Copy className="size-4" /></IconButton>
      </div>
    </Field>
  )
}

/** Scripts, WebDAV clients and S3 tools: tokens, and the address they use. */
export function ProgramsPanel({ manage }: { manage: boolean }) {
  const state = useResource(() => programsApi.get(), [])
  const tokens = useResource(() => programsApi.tokens(), [])
  const [draft, setDraft] = useState<ProgramSettings | null>(null)
  const [busy, setBusy] = useState(false)
  const [making, setMaking] = useState(false)
  const [made, setMade] = useState<CreatedToken | null>(null)

  useEffect(() => {
    if (state.data) setDraft(state.data.settings)
  }, [state.data])

  const save = async (next: ProgramSettings) => {
    setDraft(next)
    setBusy(true)
    try {
      const r = await programsApi.set(next)
      if (r.status.error) toast(r.status.error, 'error')
      state.reload()
    } catch (e) {
      toastError(e)
      state.reload()
    } finally {
      setBusy(false)
    }
  }

  const remove = async (id: string, name: string) => {
    if (!confirm(`Remove the token “${name}”? Programs using it stop working.`)) return
    try {
      await programsApi.removeToken(id)
      tokens.reload()
    } catch (e) {
      toastError(e)
    }
  }

  const status = state.data?.status
  const addr = status?.addresses[0]
  return (
    <Panel title="Programs, WebDAV and S3" className="mb-4">
      <div className="divide-y divide-line">
        <Row label="Address for programs">
          {!draft ? <Spinner /> : (
            <div className="space-y-2 text-[13px]">
              <p>Scripts can use the NEXUS API, file managers can connect with WebDAV, and backup tools such as rclone or restic can use NEXUS as S3 storage. They sign in with a token from below.</p>
              <label className="flex items-center gap-2">
                <input type="checkbox" className="accent-[var(--nx-accent)]" checked={draft.enabled} disabled={!manage || busy}
                  onChange={(e) => void save({ ...draft, enabled: e.target.checked })} />
                Let programs connect
              </label>
              {draft.enabled && (
                <>
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="text-muted">Port</span>
                    <Input type="number" className="w-28" min={1024} max={65535} value={draft.port} disabled={!manage || busy}
                      onChange={(e) => setDraft({ ...draft, port: Number(e.target.value) })}
                      onBlur={() => state.data && draft.port !== state.data.settings.port && void save(draft)} />
                  </div>
                  <label className="flex items-start gap-2">
                    <input type="checkbox" className="mt-0.5 accent-[var(--nx-accent)]" checked={draft.lan} disabled={!manage || busy}
                      onChange={(e) => void save({ ...draft, lan: e.target.checked })} />
                    <span>
                      Allow other computers on the network
                      <span className="block text-[12px] text-subtle">
                        Connections are not encrypted: anyone on the same network could see files and tokens as they pass. Use this on networks you trust.
                      </span>
                    </span>
                  </label>
                  {status?.error && <ErrorNote>{status.error}</ErrorNote>}
                  {status?.running && addr && (
                    <div className="rounded-md border border-line bg-surface p-2.5 font-mono text-[12px] leading-relaxed">
                      <div>API: {addr}/api/v1</div>
                      <div>WebDAV: {addr}/dav/</div>
                      <div>S3: {addr} (path-style, region us-east-1)</div>
                      {status.addresses.slice(1).map((a) => <div key={a} className="text-muted">From the network: {a}</div>)}
                    </div>
                  )}
                </>
              )}
            </div>
          )}
        </Row>
        <Row label="Tokens">
          <div className="space-y-2">
            {tokens.error && <ErrorNote>{tokens.error.message}</ErrorNote>}
            {tokens.data?.length === 0 && <p className="text-[13px] text-muted">No tokens yet.</p>}
            {tokens.data?.map((t) => (
              <div key={t.id} className="flex items-center gap-3 rounded-md border border-line px-3 py-2">
                <KeyRound className="size-4 shrink-0 text-muted" />
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2 text-[13px]">
                    <span className="font-medium">{t.name}</span>
                    {t.read_only && <Badge>Read only</Badge>}
                    {t.username && manage && <span className="text-subtle">{t.username}</span>}
                  </div>
                  <div className="text-[12px] text-subtle">
                    Made {formatDate(t.created_at)} · {t.last_used ? `last used ${formatRelative(t.last_used)}` : 'not used yet'}
                    {t.expires_at ? ` · stops working ${formatDate(t.expires_at)}` : ''}
                    {' · '}<span className="font-mono">{t.id}</span>
                  </div>
                </div>
                <IconButton label="Remove" onClick={() => void remove(t.id, t.name)}><Trash2 className="size-4" /></IconButton>
              </div>
            ))}
            <Button size="sm" icon={<Plus className="size-3.5" />} onClick={() => setMaking(true)}>New token</Button>
          </div>
        </Row>
      </div>
      {making && <NewTokenModal onClose={() => setMaking(false)} onMade={(t) => { setMaking(false); setMade(t); tokens.reload() }} />}
      {made && (
        <Modal open title={`Token “${made.name}”`} onClose={() => setMade(null)} footer={<Button variant="primary" onClick={() => setMade(null)}>Done</Button>}>
          <div className="space-y-3">
            <p className="text-[13px] text-warn">Copy it now: it is not shown again.</p>
            <Secret label="Token" value={made.secret_token} />
            <p className="text-[12px] text-subtle">For the API, send <span className="font-mono">Authorization: Bearer</span> and the token. For WebDAV, use any user name and the token as the password.</p>
            <Secret label="S3 access key" value={made.access_key} />
            <Secret label="S3 secret key" value={made.secret_key} />
          </div>
        </Modal>
      )}
    </Panel>
  )
}

function NewTokenModal({ onClose, onMade }: { onClose: () => void; onMade: (t: CreatedToken) => void }) {
  const [name, setName] = useState('')
  const [readOnly, setReadOnly] = useState(false)
  const [days, setDays] = useState(0)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const make = async () => {
    setBusy(true)
    setError(null)
    try {
      onMade(await programsApi.createToken({ name, read_only: readOnly, expires_days: days || undefined }))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal open title="New token" onClose={onClose}
      footer={<><Button variant="ghost" onClick={onClose}>Cancel</Button><Button variant="primary" loading={busy} disabled={!name.trim()} onClick={make}>Make token</Button></>}>
      <div className="space-y-3">
        <Field label="What will use it"><Input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="rclone on the laptop" /></Field>
        <label className="flex items-start gap-2 text-[13px]">
          <input type="checkbox" className="mt-0.5 accent-[var(--nx-accent)]" checked={readOnly} onChange={(e) => setReadOnly(e.target.checked)} />
          <span>Read only<span className="block text-[12px] text-subtle">It can list and download, but not add, change or delete.</span></span>
        </label>
        <Field label="Stops working">
          <select className="h-8 w-full rounded-md border border-line bg-panel px-2 text-[13px]" value={days} onChange={(e) => setDays(Number(e.target.value))}>
            <option value={0}>Never (until removed)</option>
            <option value={30}>In 30 days</option>
            <option value={90}>In 90 days</option>
            <option value={365}>In a year</option>
          </select>
        </Field>
        <p className="text-[12px] text-subtle">The token acts as you, with what you may see and do. It cannot manage people, tokens, pairing or sign-in.</p>
        {error && <ErrorNote>{error}</ErrorNote>}
      </div>
    </Modal>
  )
}
