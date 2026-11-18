import { useState } from 'react'
import { ArrowLeft, Fingerprint } from 'lucide-react'
import { api } from '../services/api'
import { useSession } from '../stores/session'
import { Logo } from '../components/Sidebar'
import { Button, ErrorNote, Field, Input } from '../components/ui'
import { localhostUrl, onLoopbackAddress, passkeyError, passkeysSupported, signWithPasskey } from '../lib/passkeys'

export function Login({ nodeName }: { nodeName: string }) {
  const { signIn } = useSession()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  /** Set once the password was right and a code is needed. */
  const [ticket, setTicket] = useState<string | null>(null)
  const [code, setCode] = useState('')
  const [error, setError] = useState('')
  const [busy, setBusy] = useState<'password' | 'code' | 'passkey' | null>(null)

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy('password')
    setError('')
    try {
      const reply = await api.login(username.trim(), password)
      if ('two_factor' in reply) {
        setTicket(reply.ticket)
        setCode('')
        setBusy(null)
        return
      }
      await signIn(reply.token)
    } catch (err) {
      setError(err instanceof Error && err.message !== 'authentication required' ? err.message : 'Incorrect username or password')
      setBusy(null)
    }
  }

  const submitCode = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!ticket) return
    setBusy('code')
    setError('')
    try {
      const session = await api.loginCode(ticket, code.trim())
      await signIn(session.token)
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err)
      setError(message)
      // The ticket ran out: start again from the password.
      if (message.includes('took too long')) {
        setTicket(null)
        setPassword('')
      }
      setBusy(null)
    }
  }

  const withPasskey = async () => {
    setBusy('passkey')
    setError('')
    try {
      const { challenge, rp_id } = await api.passkeyStart()
      const answer = await signWithPasskey(challenge, rp_id)
      const session = await api.passkeySignIn({ ...answer, challenge })
      await signIn(session.token)
    } catch (err) {
      setError(passkeyError(err))
      setBusy(null)
    }
  }

  const header = (
    <div className="flex items-center gap-3 pb-1">
      <Logo className="size-8" />
      <div>
        <div className="text-[15px] font-semibold">{ticket ? 'Two-step sign-in' : 'Sign in'}</div>
        <div className="text-muted">{nodeName}</div>
      </div>
    </div>
  )

  if (ticket) {
    return (
      <div className="flex min-h-full items-center justify-center bg-bg p-6">
        <form key="code" onSubmit={submitCode} className="w-full max-w-sm space-y-4 rounded-lg border border-line bg-panel p-6">
          {header}
          <Field label="Code" hint="The six-digit code your authenticator app shows for NEXUS, or one of your recovery codes.">
            <Input
              value={code}
              onChange={(e) => setCode(e.target.value)}
              autoComplete="one-time-code"
              inputMode="text"
              autoFocus
              required
              className="font-mono tracking-widest"
            />
          </Field>
          <ErrorNote>{error}</ErrorNote>
          <Button type="submit" variant="primary" className="w-full" loading={busy === 'code'}>Continue</Button>
          <button
            type="button"
            className="flex items-center gap-1 text-[12.5px] text-muted hover:text-fg"
            onClick={() => { setTicket(null); setPassword(''); setError('') }}
          >
            <ArrowLeft className="size-3.5" /> Back
          </button>
        </form>
      </div>
    )
  }

  return (
    <div className="flex min-h-full items-center justify-center bg-bg p-6">
      <form key="password" onSubmit={submit} className="w-full max-w-sm space-y-4 rounded-lg border border-line bg-panel p-6">
        {header}
        <Field label="Username">
          <Input value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username webauthn" autoFocus required />
        </Field>
        <Field label="Password">
          <Input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required />
        </Field>
        <ErrorNote>{error}</ErrorNote>
        <Button type="submit" variant="primary" className="w-full" loading={busy === 'password'}>Sign in</Button>
        {passkeysSupported() && (
          <div className="border-t border-line pt-4">
            {onLoopbackAddress() ? (
              <p className="text-[12.5px] text-subtle">
                To sign in with a passkey, open <a className="text-accent hover:underline" href={localhostUrl()}>{localhostUrl()}</a>.
              </p>
            ) : (
              <Button
                type="button"
                className="w-full"
                icon={<Fingerprint className="size-4" />}
                loading={busy === 'passkey'}
                onClick={() => void withPasskey()}
              >
                Sign in with a passkey
              </Button>
            )}
          </div>
        )}
      </form>
    </div>
  )
}
