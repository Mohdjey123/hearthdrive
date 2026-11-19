// Settings → Security: two-step sign-in, passkeys, where you are signed in,
// and the security log.

import { useEffect, useState } from 'react'
import QRCode from 'qrcode'
import { Copy, Download, Fingerprint, KeyRound, LogOut, ShieldCheck, Trash2 } from 'lucide-react'
import { api, type SecurityStatus, type SignInSession } from '../services/api'
import { useResource } from '../hooks/useResource'
import { ACTIONS } from '../lib/activity'
import { formatDate, formatRelative } from '../lib/format'
import { createPasskey, localhostUrl, onLoopbackAddress, passkeyError, passkeysSupported } from '../lib/passkeys'
import { toast, toastError } from './Toasts'
import { Badge, Button, ErrorNote, Field, Input, Modal, Panel, Spinner } from './ui'

/** Password, and a code when two-step sign-in is on: asked before sensitive changes. */
export function ConfirmFields({ twoFactor, password, code, onPassword, onCode }: {
  twoFactor: boolean
  password: string
  code: string
  onPassword: (v: string) => void
  onCode: (v: string) => void
}) {
  return (
    <>
      <Field label="Your password">
        <Input type="password" autoFocus autoComplete="current-password" value={password} onChange={(e) => onPassword(e.target.value)} />
      </Field>
      {twoFactor && (
        <Field label="Code" hint="From your authenticator app, or a recovery code.">
          <Input autoComplete="one-time-code" className="font-mono tracking-widest" value={code} onChange={(e) => onCode(e.target.value)} />
        </Field>
      )}
    </>
  )
}

/** Recovery codes, shown once, with ways to keep them. */
function RecoveryCodes({ codes, onDone }: { codes: string[]; onDone: () => void }) {
  const text = `HearthDrive recovery codes (each works once)\n\n${codes.join('\n')}\n`
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text)
      toast('Copied the recovery codes')
    } catch (e) {
      toastError(e)
    }
  }
  const download = () => {
    const url = URL.createObjectURL(new Blob([text], { type: 'text/plain' }))
    const a = document.createElement('a')
    a.href = url
    a.download = 'nexus-recovery-codes.txt'
    a.click()
    URL.revokeObjectURL(url)
  }
  return (
    <Modal
      open
      onClose={onDone}
      title="Your recovery codes"
      footer={<Button variant="primary" onClick={onDone}>I have kept them</Button>}
    >
      <div className="space-y-3">
        <p className="text-muted">
          If your phone is lost or reset, sign in with one of these instead of a code. Each works once. They are shown only now: keep them
          somewhere other than this computer, such as a password manager or on paper.
        </p>
        <ul className="grid grid-cols-2 gap-x-6 gap-y-1 rounded-md border border-line bg-bg p-3 font-mono text-[14px] tracking-wide select-all">
          {codes.map((c) => <li key={c}>{c}</li>)}
        </ul>
        <div className="flex gap-2">
          <Button size="sm" icon={<Copy className="size-3.5" />} onClick={() => void copy()}>Copy</Button>
          <Button size="sm" icon={<Download className="size-3.5" />} onClick={download}>Save as a file</Button>
        </div>
      </div>
    </Modal>
  )
}

/** Turning on two-step sign-in: password, scan the QR code, enter a code. */
function TurnOnTwoFactor({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const [password, setPassword] = useState('')
  const [setup, setSetup] = useState<{ secret: string; uri: string } | null>(null)
  const [qr, setQr] = useState('')
  const [code, setCode] = useState('')
  const [codes, setCodes] = useState<string[] | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  useEffect(() => {
    if (setup) void QRCode.toDataURL(setup.uri, { margin: 2, width: 440, errorCorrectionLevel: 'M', color: { dark: '#000000', light: '#ffffff' } }).then(setQr)
  }, [setup])

  if (codes) return <RecoveryCodes codes={codes} onDone={onDone} />

  const next = async () => {
    setBusy(true)
    setError('')
    try {
      if (!setup) setSetup(await api.setupTwoFactor(password))
      else {
        setCodes((await api.enableTwoFactor(code.trim())).recovery_codes)
        toast('Two-step sign-in is on')
      }
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      open
      onClose={onClose}
      title="Turn on two-step sign-in"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={setup ? code.trim().length < 6 : !password} onClick={() => void next()}>
            {setup ? 'Turn on' : 'Continue'}
          </Button>
        </>
      }
    >
      <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); void next() }}>
        {!setup ? (
          <>
            <p className="text-muted">
              After your password, signing in will also ask for a six-digit code from an authenticator app on your phone (such as
              Microsoft Authenticator, Google Authenticator, 2FAS or Aegis).
            </p>
            <Field label="Your password">
              <Input type="password" autoFocus autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} />
            </Field>
          </>
        ) : (
          <>
            <p className="text-muted">Scan this with your authenticator app, then enter the code it shows.</p>
            <div className="flex justify-center">
              {qr ? <img src={qr} alt="QR code for your authenticator app" className="size-48 rounded-md" /> : <Spinner />}
            </div>
            <p className="text-center text-[12px] text-subtle">
              Can't scan? Enter this key in the app: <span className="font-mono text-fg select-all">{setup.secret.replace(/(.{4})/g, '$1 ').trim()}</span>
            </p>
            <Field label="Code from the app">
              <Input autoFocus inputMode="numeric" autoComplete="one-time-code" className="font-mono tracking-widest" value={code} onChange={(e) => setCode(e.target.value)} />
            </Field>
          </>
        )}
        <ErrorNote>{error}</ErrorNote>
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}

/** Turning two-step sign-in off, or making new recovery codes. */
function ConfirmTwoFactorChange({ mode, onClose, onDone }: { mode: 'off' | 'codes'; onClose: () => void; onDone: () => void }) {
  const [password, setPassword] = useState('')
  const [code, setCode] = useState('')
  const [codes, setCodes] = useState<string[] | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  if (codes) return <RecoveryCodes codes={codes} onDone={onDone} />
  const go = async () => {
    setBusy(true)
    setError('')
    try {
      if (mode === 'off') {
        await api.disableTwoFactor(password, code.trim())
        toast('Two-step sign-in is off')
        onDone()
      } else {
        setCodes((await api.newRecoveryCodes(password, code.trim())).recovery_codes)
      }
    } catch (e) {
      setError((e as Error).message)
      setBusy(false)
    }
  }
  return (
    <Modal
      open
      onClose={onClose}
      title={mode === 'off' ? 'Turn off two-step sign-in?' : 'New recovery codes'}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant={mode === 'off' ? 'danger' : 'primary'} loading={busy} disabled={!password || !code.trim()} onClick={() => void go()}>
            {mode === 'off' ? 'Turn off' : 'Make new codes'}
          </Button>
        </>
      }
    >
      <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); void go() }}>
        <p className="text-muted">
          {mode === 'off'
            ? 'Signing in will only ask for your password again. Your recovery codes stop working.'
            : 'Your current recovery codes stop working and ten new ones are shown.'}
        </p>
        <ConfirmFields twoFactor password={password} code={code} onPassword={setPassword} onCode={setCode} />
        <ErrorNote>{error}</ErrorNote>
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}

function AddPasskey({ twoFactor, onClose, onDone }: { twoFactor: boolean; onClose: () => void; onDone: () => void }) {
  const [name, setName] = useState(navigator.userAgent.includes('Windows') ? 'Windows Hello' : 'Passkey')
  const [password, setPassword] = useState('')
  const [code, setCode] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const add = async () => {
    setBusy(true)
    setError('')
    try {
      const options = await api.addPasskeyStart(password, twoFactor ? code.trim() : undefined)
      const made = await createPasskey(options)
      await api.addPasskey({ challenge: options.challenge, ...made, name: name.trim() })
      toast('Added the passkey')
      onDone()
    } catch (e) {
      setError(passkeyError(e))
      setBusy(false)
    }
  }
  return (
    <Modal
      open
      onClose={onClose}
      title="Add a passkey"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!password || (twoFactor && !code.trim())} onClick={() => void add()}>Continue</Button>
        </>
      }
    >
      <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); void add() }}>
        <p className="text-muted">
          Sign in with Windows Hello (face, fingerprint or PIN), your phone or a security key instead of a password and code. Your browser
          asks where to keep the passkey.
        </p>
        <Field label="Name" hint="To tell your passkeys apart.">
          <Input value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
        </Field>
        <ConfirmFields twoFactor={twoFactor} password={password} code={code} onPassword={setPassword} onCode={setCode} />
        <ErrorNote>{error}</ErrorNote>
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}

/** "Edge on Windows" from a user agent. */
export function describeClient(ua: string | null): string {
  if (!ua) return 'Unknown browser'
  const browser = /Edg\//.test(ua) ? 'Edge' : /OPR\//.test(ua) ? 'Opera' : /Firefox\//.test(ua) ? 'Firefox' : /Chrome\//.test(ua) ? 'Chrome' : /Safari\//.test(ua) ? 'Safari' : /curl|python|reqwest/i.test(ua) ? 'A script' : 'A browser'
  const os = /Windows/.test(ua) ? 'Windows' : /Android/.test(ua) ? 'Android' : /iPhone|iPad/.test(ua) ? 'iOS' : /Mac OS X/.test(ua) ? 'macOS' : /Linux/.test(ua) ? 'Linux' : ''
  return os ? `${browser} on ${os}` : browser
}

const METHODS: Record<string, string> = {
  password: 'password',
  'password+code': 'password and code',
  'password+recovery': 'password and recovery code',
  passkey: 'passkey',
  setup: 'setup',
}

function SessionRow({ s, onEnd }: { s: SignInSession; onEnd: (s: SignInSession) => void }) {
  return (
    <li className="flex items-center gap-3 py-2">
      <div className="min-w-0 flex-1">
        <div className="truncate font-medium">
          {describeClient(s.client)} {s.current && <Badge tone="accent">This browser</Badge>}
        </div>
        <div className="truncate text-[12px] text-subtle">
          Signed in {formatRelative(s.created_at)}{s.method ? ` with ${METHODS[s.method] ?? s.method}` : ''} · last used {formatRelative(s.last_used_at ?? s.created_at)}
        </div>
      </div>
      {!s.current && (
        <Button size="sm" variant="ghost" className="hover:text-danger" icon={<LogOut className="size-3.5" />} onClick={() => onEnd(s)}>
          Sign out
        </Button>
      )}
    </li>
  )
}

function Log() {
  const [limit, setLimit] = useState(15)
  const log = useResource(() => api.securityLog(limit), [limit])
  if (log.error) return <ErrorNote>{log.error.message}</ErrorNote>
  if (!log.data) return <Spinner />
  if (!log.data.length) return <p className="text-subtle">Nothing yet.</p>
  return (
    <>
      <ul className="divide-y divide-line">
        {log.data.map((e) => (
          <li key={e.id} className="flex items-baseline gap-3 py-1.5 text-[12.5px]">
            <span className={e.action === 'auth.failed' ? 'w-44 shrink-0 font-medium text-danger' : 'w-44 shrink-0 font-medium'}>{ACTIONS[e.action] ?? e.action}</span>
            <span className="min-w-0 flex-1 truncate text-muted">
              {[e.username, e.resource].filter(Boolean).join(' · ')}
            </span>
            <span className="shrink-0 text-subtle" title={formatDate(e.timestamp)}>{formatRelative(e.timestamp)}</span>
          </li>
        ))}
      </ul>
      {log.data.length >= limit && limit < 500 && (
        <Button size="sm" variant="ghost" className="mt-1" onClick={() => setLimit(limit * 4)}>Show more</Button>
      )}
    </>
  )
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-1 items-start gap-1 py-3 md:grid-cols-[200px_1fr] md:gap-4">
      <div className="text-muted">{label}</div>
      <div className="min-w-0 break-words">{children}</div>
    </div>
  )
}

function PasskeysHere({ status, onAdd }: { status: SecurityStatus; onAdd: () => void }) {
  if (!passkeysSupported()) return <p className="text-[12px] text-subtle">This browser cannot use passkeys.</p>
  if (onLoopbackAddress())
    return (
      <p className="text-[12px] text-subtle">
        To add a passkey, open <a className="text-accent hover:underline" href={localhostUrl()}>{localhostUrl()}</a> (passkeys need a name, not a number).
      </p>
    )
  if (!status.passkeys_here) return <p className="text-[12px] text-subtle">Passkeys cannot be added from this page.</p>
  return <Button size="sm" icon={<Fingerprint className="size-3.5" />} onClick={onAdd}>Add a passkey</Button>
}

export function SecurityPanel() {
  const status = useResource(() => api.security(), [])
  const [dialog, setDialog] = useState<'on' | 'off' | 'codes' | 'passkey' | null>(null)
  const [busy, setBusy] = useState(false)
  const done = () => {
    setDialog(null)
    status.reload()
  }

  const removePasskey = async (id: string, name: string) => {
    try {
      await api.removePasskey(id)
      toast(`Removed the passkey "${name}"`)
    } catch (e) {
      toastError(e)
    }
    status.reload()
  }
  const endSession = async (s: SignInSession) => {
    try {
      await api.endSession(s.id)
      toast(`Signed out ${describeClient(s.client)}`)
    } catch (e) {
      toastError(e)
    }
    status.reload()
  }
  const endOthers = async () => {
    setBusy(true)
    try {
      const { ended } = await api.endOtherSessions()
      toast(ended ? `Signed out ${ended} other session${ended === 1 ? '' : 's'}` : 'No other sessions')
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
      status.reload()
    }
  }

  const s = status.data
  const others = s?.sessions.filter((x) => !x.current).length ?? 0
  return (
    <Panel title="Security" className="mb-4">
      {status.error ? (
        <ErrorNote>{status.error.message}</ErrorNote>
      ) : !s ? (
        <Spinner />
      ) : (
        <div className="divide-y divide-line">
          <Row label="Two-step sign-in">
            {s.two_factor ? (
              <div className="space-y-2">
                <div className="flex items-center gap-2">
                  <ShieldCheck className="size-4 text-accent" /> On since {formatDate(s.two_factor_since)}
                </div>
                <div className={s.recovery_codes_left <= 3 ? 'text-[12px] text-warn' : 'text-[12px] text-subtle'}>
                  {s.recovery_codes_left} recovery code{s.recovery_codes_left === 1 ? '' : 's'} left
                  {s.recovery_codes_left <= 3 && ': make new ones before they run out'}
                </div>
                <div className="flex flex-wrap gap-2">
                  <Button size="sm" icon={<KeyRound className="size-3.5" />} onClick={() => setDialog('codes')}>New recovery codes</Button>
                  <Button size="sm" variant="ghost" className="hover:text-danger" onClick={() => setDialog('off')}>Turn off</Button>
                </div>
              </div>
            ) : (
              <div className="space-y-2">
                <p className="text-muted">Off. Signing in from a browser needs only your password.</p>
                <Button size="sm" variant="primary" icon={<ShieldCheck className="size-3.5" />} onClick={() => setDialog('on')}>Turn on</Button>
              </div>
            )}
          </Row>
          <Row label="Passkeys">
            <div className="space-y-2">
              {s.passkeys.length > 0 && (
                <ul className="divide-y divide-line rounded-md border border-line">
                  {s.passkeys.map((p) => (
                    <li key={p.id} className="flex items-center gap-3 px-3 py-2">
                      <Fingerprint className="size-4 shrink-0 text-muted" />
                      <div className="min-w-0 flex-1">
                        <div className="truncate font-medium">{p.name}</div>
                        <div className="text-[12px] text-subtle">
                          Added {formatDate(p.created_at)} · {p.last_used_at ? `last used ${formatRelative(p.last_used_at)}` : 'not used yet'}
                        </div>
                      </div>
                      <Button size="sm" variant="ghost" className="hover:text-danger" icon={<Trash2 className="size-3.5" />} onClick={() => void removePasskey(p.id, p.name)}>
                        Remove
                      </Button>
                    </li>
                  ))}
                </ul>
              )}
              <PasskeysHere status={s} onAdd={() => setDialog('passkey')} />
            </div>
          </Row>
          <Row label="Signed in">
            {s.sessions.length === 0 ? (
              <p className="text-muted">Only in this app, which signs in by itself.</p>
            ) : (
              <>
                <ul className="divide-y divide-line">
                  {s.sessions.map((x) => <SessionRow key={x.id} s={x} onEnd={(x) => void endSession(x)} />)}
                </ul>
                {others > 0 && (
                  <Button size="sm" className="mt-2" loading={busy} icon={<LogOut className="size-3.5" />} onClick={() => void endOthers()}>
                    Sign out everywhere else
                  </Button>
                )}
              </>
            )}
          </Row>
          <Row label="Security log">
            <Log />
          </Row>
        </div>
      )}
      {dialog === 'on' && <TurnOnTwoFactor onClose={() => setDialog(null)} onDone={done} />}
      {(dialog === 'off' || dialog === 'codes') && <ConfirmTwoFactorChange mode={dialog} onClose={() => setDialog(null)} onDone={done} />}
      {dialog === 'passkey' && s && <AddPasskey twoFactor={s.two_factor} onClose={() => setDialog(null)} onDone={done} />}
    </Panel>
  )
}
