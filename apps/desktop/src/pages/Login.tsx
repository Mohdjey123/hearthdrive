import { useState } from 'react'
import { api } from '../services/api'
import { useSession } from '../stores/session'
import { Logo } from '../components/Sidebar'
import { Button, ErrorNote, Field, Input } from '../components/ui'

export function Login({ nodeName }: { nodeName: string }) {
  const { signIn } = useSession()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setError('')
    try {
      const session = await api.login(username.trim(), password)
      await signIn(session.token)
    } catch (err) {
      setError(err instanceof Error && err.message !== 'authentication required' ? err.message : 'Incorrect username or password')
      setBusy(false)
    }
  }

  return (
    <div className="flex min-h-full items-center justify-center bg-bg p-6">
      <form onSubmit={submit} className="w-full max-w-sm space-y-4 rounded-lg border border-line bg-panel p-6">
        <div className="flex items-center gap-3 pb-1">
          <Logo className="size-8" />
          <div>
            <div className="text-[15px] font-semibold">Sign in</div>
            <div className="text-muted">{nodeName}</div>
          </div>
        </div>
        <Field label="Username">
          <Input value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username" autoFocus required />
        </Field>
        <Field label="Password">
          <Input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required />
        </Field>
        <ErrorNote>{error}</ErrorNote>
        <Button type="submit" variant="primary" className="w-full" loading={busy}>Sign in</Button>
      </form>
    </div>
  )
}
