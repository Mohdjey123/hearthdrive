import { useState } from 'react'
import { Check, Cloud, HardDrive, Link2, Smartphone } from 'lucide-react'
import { api, isMobile, setSessionToken, type JoinState, type StorageRoot, type SystemInfo } from '../services/api'
import { JoinForm } from '../components/Pairing'
import { useSession } from '../stores/session'
import { AddStorageForm } from '../components/AddStorage'
import { Logo } from '../components/Sidebar'
import { Badge, Button, ErrorNote, Field, Input, cx } from '../components/ui'

const STEPS = ['Create', 'Account', 'Storage', 'Finish']
const JOIN_STEPS = ['Join', 'Account', 'Pair & storage', 'Finish']

function Choice({ icon, title, body, tag, disabled, selected, onClick }: {
  icon: React.ReactNode
  title: string
  body: string
  tag?: React.ReactNode
  disabled?: boolean
  selected?: boolean
  onClick?: () => void
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={cx(
        'flex w-full items-start gap-3 rounded-lg border p-4 text-left transition-colors',
        selected ? 'border-accent bg-accent-soft' : 'border-line bg-elevated',
        disabled ? 'cursor-not-allowed opacity-55' : 'hover:border-line-strong',
      )}
    >
      <div className="mt-0.5 text-accent">{icon}</div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2 font-medium">
          {title} {tag}
        </div>
        <div className="mt-0.5 text-muted">{body}</div>
      </div>
    </button>
  )
}

export function Setup({ defaultName }: { defaultName: string }) {
  return isMobile() ? <MobileSetup defaultName={defaultName} /> : <DesktopSetup defaultName={defaultName} />
}

/** A password nobody has to remember: the phone app signs in to its own
 *  node with a per-run token, so the account password is never typed. */
function randomPassword(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(24))
  return btoa(String.fromCharCode(...bytes))
}

/** Phone setup: name the phone, then pair it with a computer. */
function MobileSetup({ defaultName }: { defaultName: string }) {
  const { reload } = useSession()
  const [step, setStep] = useState<'name' | 'pair' | 'done'>('name')
  const [name, setName] = useState(defaultName || 'My phone')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [joined, setJoined] = useState<JoinState | null>(null)

  const create = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setError('')
    try {
      const session = await api.setup({ node_name: name.trim(), username: 'owner', password: randomPassword() })
      setSessionToken(session.token)
      setStep('pair')
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="safe-area flex min-h-full flex-col bg-bg">
      <div className="flex flex-1 flex-col justify-center p-5">
        <div className="mb-6 flex items-center gap-3">
          <Logo className="size-9" />
          <div>
            <div className="text-[17px] font-semibold tracking-tight">Hearthdrive</div>
            <div className="text-muted">Your devices. Your storage. One cloud.</div>
          </div>
        </div>

        <div className="rounded-lg border border-line bg-panel p-5">
          {step === 'name' && (
            <form onSubmit={create} className="space-y-4">
              <div>
                <h2 className="text-[15px] font-semibold">Set up this phone</h2>
                <p className="mt-1 text-muted">
                  Your files stay on your computers. This phone connects to them directly and securely — no account, no cloud service.
                </p>
              </div>
              <Field label="Phone name" hint="Shown on your computers when you pair.">
                <Input value={name} onChange={(e) => setName(e.target.value)} maxLength={64} className="h-10" />
              </Field>
              <ErrorNote>{error}</ErrorNote>
              <Button type="submit" variant="primary" className="h-11 w-full" loading={busy} disabled={!name.trim()}>Continue</Button>
            </form>
          )}
          {step === 'pair' && (
            <div className="space-y-4">
              <h2 className="text-[15px] font-semibold">Pair with a computer</h2>
              <JoinForm onJoined={(j) => { setJoined(j); setStep('done') }} />
              <button className="block w-full pt-1 text-center text-[12.5px] text-muted underline" onClick={() => setStep('done')}>
                Skip for now
              </button>
            </div>
          )}
          {step === 'done' && (
            <div className="space-y-4 text-center">
              <div className="mx-auto w-fit rounded-full bg-accent-soft p-3"><Check className="size-6 text-accent" /></div>
              <h2 className="text-[15px] font-semibold">{joined ? `Paired with ${joined.host_name}` : 'This phone is ready'}</h2>
              <p className="text-muted">
                {joined
                  ? 'You can now browse, download and upload its files from anywhere.'
                  : 'Pair it with a computer from Devices whenever you like.'}
              </p>
              <Button variant="primary" className="h-11 w-full" onClick={() => void reload()}>Open Hearthdrive</Button>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

function DesktopSetup({ defaultName }: { defaultName: string }) {
  const { reload } = useSession()
  const [step, setStep] = useState(0)
  const [nodeName, setNodeName] = useState(defaultName)
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const [roots, setRoots] = useState<StorageRoot[]>([])
  const [info, setInfo] = useState<SystemInfo | null>(null)
  const [mode, setMode] = useState<'create' | 'join'>('create')
  const [joined, setJoined] = useState<JoinState | null>(null)
  const [skippedJoin, setSkippedJoin] = useState(false)

  const createAccount = async (e: React.FormEvent) => {
    e.preventDefault()
    if (password !== confirm) {
      setError('The passwords do not match')
      return
    }
    setBusy(true)
    setError('')
    try {
      const session = await api.setup({ node_name: nodeName.trim(), username: username.trim(), password })
      setSessionToken(session.token)
      setStep(2)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  const finish = async () => {
    setStep(3)
    try { setInfo(await api.systemInfo()) } catch { /* optional */ }
  }

  return (
    <div className="flex min-h-full items-center justify-center bg-bg p-6">
      <div className="w-full max-w-xl">
        <div className="mb-6 flex items-center gap-3">
          <Logo className="size-8" />
          <div>
            <div className="text-[16px] font-semibold tracking-tight">Hearthdrive setup</div>
            <div className="text-muted">Your devices. Your storage. One cloud.</div>
          </div>
        </div>

        <ol className="mb-5 flex gap-1.5">
          {(mode === 'join' ? JOIN_STEPS : STEPS).map((label, i) => (
            <li key={label} className="flex-1">
              <div className={cx('h-1 rounded-full', i <= step ? 'bg-accent' : 'bg-elevated')} />
              <div className={cx('mt-1.5 text-[11.5px]', i === step ? 'text-fg' : 'text-subtle')}>
                {i + 1}. {label}
              </div>
            </li>
          ))}
        </ol>

        <div className="rounded-lg border border-line bg-panel p-6">
          {step === 0 && (
            <div className="space-y-3">
              <h2 className="text-[15px] font-semibold">Welcome to your personal cloud</h2>
              <p className="pb-1 text-muted">Choose how this computer will participate.</p>
              <Choice
                icon={<Cloud className="size-5" />}
                title="Create my cloud"
                body="Make this computer a storage node."
                tag={<Badge tone="accent">Recommended</Badge>}
                selected={mode === 'create'}
                onClick={() => setMode('create')}
              />
              <Choice
                icon={<Link2 className="size-5" />}
                title="Join an existing cloud"
                body="Pair this computer with one that already runs Hearthdrive, using its pairing link."
                selected={mode === 'join'}
                onClick={() => setMode('join')}
              />
              <Choice
                icon={<Smartphone className="size-5" />}
                title="Connect my phone"
body="Install the Hearthdrive app on an Android phone and scan this computer's pairing code. Set up this computer first, then use Devices → Add device."
                tag={<Badge>From the phone app</Badge>}
                disabled
              />
              <div className="flex justify-end pt-2">
                <Button variant="primary" onClick={() => setStep(1)}>Continue</Button>
              </div>
            </div>
          )}

          {step === 1 && (
            <form onSubmit={createAccount} className="space-y-4">
              <div>
                <h2 className="text-[15px] font-semibold">Name this computer and create your account</h2>
                <p className="mt-1 text-muted">
                  Your account is stored only on this computer. No Google, Microsoft or other online account is needed.
                </p>
              </div>
              <Field label="Computer name">
                <Input value={nodeName} onChange={(e) => setNodeName(e.target.value)} maxLength={64} required />
              </Field>
              <Field label="Username" hint="3–32 letters, digits, '.', '_' or '-'">
                <Input value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username" required />
              </Field>
              <div className="grid grid-cols-2 gap-3">
                <Field label="Password">
                  <Input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" minLength={8} required />
                </Field>
                <Field label="Confirm password">
                  <Input type="password" value={confirm} onChange={(e) => setConfirm(e.target.value)} autoComplete="new-password" required />
                </Field>
              </div>
              <p className="text-[12px] text-subtle">At least 8 characters. Stored as an Argon2id hash, never in plain text.</p>
              <ErrorNote>{error}</ErrorNote>
              <div className="flex justify-between pt-1">
                <Button type="button" variant="ghost" onClick={() => setStep(0)}>Back</Button>
                <Button type="submit" variant="primary" loading={busy}>Create account</Button>
              </div>
            </form>
          )}

          {step === 2 && mode === 'join' && !joined && !skippedJoin && (
            <div className="space-y-4">
              <div>
                <h2 className="text-[15px] font-semibold">Join your cloud</h2>
                <p className="mt-1 text-muted">Pair with a computer that already runs Hearthdrive. Its owner approves the request there.</p>
              </div>
              <JoinForm onJoined={setJoined} />
              <div className="border-t border-line pt-3 text-right">
                <Button variant="ghost" size="sm" onClick={() => setSkippedJoin(true)}>Skip — pair later from Devices</Button>
              </div>
            </div>
          )}

          {step === 2 && (mode === 'create' || joined || skippedJoin) && (
            <div className="space-y-4">
              {joined && (
                <div className="flex items-center gap-2 rounded-md bg-accent-soft px-3 py-2">
                  <Check className="size-4 text-accent" /> Paired with <span className="font-medium">{joined.host_name}</span>
                </div>
              )}
              <div>
                <h2 className="text-[15px] font-semibold">Choose folders and drives</h2>
                <p className="mt-1 text-muted">
                  Add the locations this computer contributes to your cloud. They are indexed in place — nothing is moved.
                </p>
              </div>
              {roots.length > 0 && (
                <ul className="space-y-1.5">
                  {roots.map((r) => (
                    <li key={r.id} className="flex items-center gap-2 rounded-md bg-elevated px-3 py-2">
                      <Check className="size-4 text-accent" />
                      <span className="font-medium">{r.name}</span>
                      <span className="truncate text-muted">{r.absolute_path}</span>
                    </li>
                  ))}
                </ul>
              )}
              <AddStorageForm onAdded={(r) => setRoots((rs) => [...rs, r])} submitLabel={roots.length ? 'Add another' : 'Add storage'} />
              <div className="flex justify-between border-t border-line pt-4">
                <span className="self-center text-[12px] text-subtle">You can add more later under Storage.</span>
                <Button variant="primary" onClick={finish}>{roots.length ? 'Continue' : 'Skip for now'}</Button>
              </div>
            </div>
          )}

          {step === 3 && (
            <div className="space-y-4">
              <h2 className="text-[15px] font-semibold">Your cloud is ready</h2>
              <ul className="space-y-2">
                <li className="flex items-start gap-2.5">
                  <Check className="mt-0.5 size-4 text-accent" />
                  <span><span className="font-medium">{nodeName}</span> is now a storage node running in the background.</span>
                </li>
                <li className="flex items-start gap-2.5">
                  <HardDrive className="mt-0.5 size-4 text-accent" />
                  <span>{roots.length ? `${roots.length} storage location${roots.length > 1 ? 's are' : ' is'} being indexed.` : 'No storage added yet.'}</span>
                </li>
                <li className="flex items-start gap-2.5">
                  <Link2 className={cx('mt-0.5 size-4', info?.network.status.connected ? 'text-accent' : 'text-subtle')} />
                  <span className={info?.network.status.connected ? '' : 'text-muted'}>
                    {joined
                      ? `Paired with ${joined.host_name}. Browse its files from Files.`
                      : info?.network.status.connected
                        ? 'Your other computers can connect from anywhere. Add them from Devices.'
                        : 'Remote access is not available yet. Devices shows the connection status.'}
                  </span>
                </li>
              </ul>
              <div className="flex justify-end pt-2">
                <Button variant="primary" data-autofocus onClick={() => void reload()}>Open Hearthdrive</Button>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
