import { useCallback, useState } from 'react'
import { FolderOpen, HardDrive, Lock, LockOpen, LogOut, Monitor, Moon, Sun, Trash2, UserPlus } from 'lucide-react'
import { api, closeAction, connection, fileApi, isMobile, libraryApi, isTauri, peerPrefix, setCloseAction, setTerminalEnabled, setTerminalShell, terminalApi, testTerminalShell, type TerminalInfo, thisDevice, type CloseAction, type Device, type Person, type Role, type StorageRoot, type User, type VirtualDriveStatus } from '../services/api'
import { SecurityPanel } from '../components/Security'
import { ProgramsPanel } from '../components/Programs'
import { toast, toastError } from '../components/Toasts'
import { useResource } from '../hooks/useResource'
import { useSession } from '../stores/session'
import { useTheme, type ThemePref } from '../stores/theme'
import { formatBytes, formatDuration } from '../lib/format'
import { UpdateSettings } from '../components/Updates'
import { PhoneStorageSettings } from '../components/PhoneStorage'
import { notifyLevel, setNotifyLevel, type NotifyLevel } from '../services/notify'
import { Badge, Button, ErrorNote, Field, Input, Modal, PageHeader, Panel, Spinner, cx } from '../components/ui'

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-1 items-start gap-1 py-3 md:grid-cols-[200px_1fr] md:gap-4">
      <div className="text-muted">{label}</div>
      <div className="min-w-0 break-words">{children}</div>
    </div>
  )
}

const ROLES: { value: Role; label: string; can: string }[] = [
  { value: 'administrator', label: 'Administrator', can: 'Everything except changing owners' },
  { value: 'editor', label: 'Editor', can: 'Browse, upload, rename, move and delete files' },
  { value: 'viewer', label: 'Viewer', can: 'Browse and download only' },
]

const SEES_ALL: Role[] = ['owner', 'administrator']

function accessLabel(u: User) {
  if (SEES_ALL.includes(u.role) || u.allowed_roots === null) return 'All storage'
  const n = u.allowed_roots.length
  return n === 0 ? 'No storage' : `${n} location${n === 1 ? '' : 's'}`
}

/** One device's storage locations, as checkboxes. */
function DeviceRoots({ device, chosen, disabled, onToggle, onLoaded }: {
  device: Device
  chosen: Set<string>
  disabled: boolean
  onToggle: (id: string, on: boolean) => void
  onLoaded: (deviceId: string, ids: string[]) => void
}) {
  const roots = useResource(async () => {
    const list: StorageRoot[] = device.is_local ? await api.roots() : await fileApi(peerPrefix(device.id)).roots()
    onLoaded(device.id, list.map((r) => r.id))
    return list
  }, [device.id])
  const name = device.is_local ? `This ${thisDevice()}` : device.name

  return (
    <div>
      <div className="mb-1 flex items-center gap-1.5 text-[12px] font-medium uppercase tracking-wide text-subtle">
        <HardDrive className="size-3.5" /> {name}
      </div>
      {roots.loading && !roots.data ? (
        <Spinner />
      ) : roots.error ? (
        <p className="text-[12.5px] text-subtle">
          {device.online ? `Could not list its storage: ${roots.error.message}` : 'Offline: its storage can be chosen when it is back.'}
        </p>
      ) : !roots.data?.length ? (
        <p className="text-[12.5px] text-subtle">No storage.</p>
      ) : (
        <ul className="divide-y divide-line rounded-md border border-line">
          {roots.data.map((r) => (
            <li key={r.id}>
              <label className="flex items-center gap-2.5 px-3 py-2">
                <input
                  type="checkbox"
                  className="accent-[var(--nx-accent)]"
                  disabled={disabled}
                  checked={chosen.has(r.id)}
                  onChange={(e) => onToggle(r.id, e.target.checked)}
                />
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-medium">{r.name}</span>
                  {device.is_local && <span className="block truncate text-[12px] text-subtle">{r.absolute_path}</span>}
                </span>
              </label>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

/** Chooses which storage, here and on paired devices, a person may use. */
function PersonAccess({ user, onClose, onSaved }: { user: User; onClose: () => void; onSaved: () => void }) {
  const devices = useResource(() => api.devices(), [])
  const [all, setAll] = useState(user.allowed_roots === null)
  const [chosen, setChosen] = useState<Set<string>>(new Set(user.allowed_roots ?? []))
  const [shown, setShown] = useState<Record<string, string[]>>({})
  const [busy, setBusy] = useState(false)

  const onLoaded = useCallback((deviceId: string, ids: string[]) => setShown((prev) => ({ ...prev, [deviceId]: ids })), [])
  const toggle = (id: string, on: boolean) =>
    setChosen((prev) => {
      const next = new Set(prev)
      if (on) next.add(id)
      else next.delete(id)
      return next
    })
  // Locations chosen earlier on devices that cannot be listed now stay chosen.
  const visible = new Set(Object.values(shown).flat())
  const hidden = [...chosen].filter((id) => !visible.has(id)).length
  const list = (devices.data ?? []).slice().sort((a, b) => Number(b.is_local) - Number(a.is_local) || a.name.localeCompare(b.name))

  const save = async () => {
    setBusy(true)
    try {
      await api.updateUser(user.id, all ? { all_roots: true } : { allowed_roots: [...chosen] })
      toast(all ? `${user.username} can use all storage` : `${user.username} can use ${chosen.size} location${chosen.size === 1 ? '' : 's'}`)
      onSaved()
      onClose()
    } catch (e) {
      toastError(e)
      setBusy(false)
    }
  }

  return (
    <Modal
      open
      onClose={onClose}
      title={`What ${user.username} can use`}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} onClick={() => void save()}>Save</Button>
        </>
      }
    >
      <div className="space-y-3">
        <p className="text-muted">
          Choose which storage {user.username} can browse, search and change, on this {thisDevice()} and on your paired devices.
          Everything else stays invisible to them, including in search, sync and the activity log.
        </p>
        <label className="flex items-center gap-2.5">
          <input type="radio" name="person-access" className="accent-[var(--nx-accent)]" checked={all} onChange={() => setAll(true)} />
          <span>All storage, including locations added later</span>
        </label>
        <label className="flex items-center gap-2.5">
          <input type="radio" name="person-access" className="accent-[var(--nx-accent)]" checked={!all} onChange={() => setAll(false)} />
          <span>Only these locations</span>
        </label>
        <div className={cx('max-h-[50vh] space-y-3 overflow-y-auto', all && 'pointer-events-none opacity-50')}>
          {devices.loading && !devices.data ? (
            <Spinner />
          ) : devices.error ? (
            <ErrorNote>{devices.error.message}</ErrorNote>
          ) : (
            list.map((d) => <DeviceRoots key={d.id} device={d} chosen={chosen} disabled={all} onToggle={toggle} onLoaded={onLoaded} />)
          )}
          {hidden > 0 && !all && (
            <p className="text-[12px] text-subtle">
              Also keeps {hidden} location{hidden === 1 ? '' : 's'} on devices that cannot be reached right now.
            </p>
          )}
        </div>
      </div>
    </Modal>
  )
}

/** Accounts that can sign in to this computer's HearthDrive from a browser on it. */
function People({ meId }: { meId: string | null }) {
  const users = useResource(() => api.users(), [])
  const [adding, setAdding] = useState(false)
  const [removing, setRemoving] = useState<User | null>(null)
  const [access, setAccess] = useState<User | null>(null)
  const [locking, setLocking] = useState<Person | null>(null)
  const [resetting, setResetting] = useState<Person | null>(null)
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [role, setRole] = useState<Role>('viewer')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  const add = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setError('')
    try {
      await api.createUser({ username: username.trim(), password, role })
      toast(`Added ${username.trim()}`)
      setAdding(false)
      setUsername('')
      setPassword('')
      users.reload()
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  const changeRole = async (u: User, next: Role) => {
    try {
      await api.updateUser(u.id, { role: next })
      toast(`${u.username} is now ${next === 'administrator' || next === 'editor' ? 'an' : 'a'} ${next}`)
    } catch (e) {
      toastError(e)
    }
    users.reload()
  }

  const change = async (u: Person, b: { locked?: boolean; two_factor?: false }, done: string) => {
    try {
      await api.updateUser(u.id, b)
      toast(done)
      setLocking(null)
      setResetting(null)
    } catch (e) {
      toastError(e)
    }
    users.reload()
  }

  const remove = async () => {
    if (!removing) return
    try {
      await api.removeUser(removing.id)
      toast(`Removed ${removing.username}`)
      setRemoving(null)
      users.reload()
    } catch (e) {
      toastError(e)
    }
  }

  return (
    <Panel
      title="People"
      className="mb-4"
      padded={false}
      actions={<Button size="sm" variant="ghost" icon={<UserPlus className="size-3.5" />} onClick={() => setAdding(true)}>Add person</Button>}
    >
      {users.error ? (
        <div className="p-4"><ErrorNote>{users.error.message}</ErrorNote></div>
      ) : !users.data ? (
        <div className="p-4"><Spinner /></div>
      ) : (
        <ul className="divide-y divide-line">
          {users.data.map((u) => (
            <li key={u.id} className="flex items-center gap-3 px-4 py-2.5">
              <span className="min-w-0 flex-1 truncate font-medium">
                {u.username} {u.id === meId && <span className="font-normal text-subtle">(you)</span>}
                {u.locked_at !== null && <> <Badge tone="danger">Locked</Badge></>}
                {u.two_factor && <> <Badge>Two-step</Badge></>}
                {u.passkeys > 0 && <> <Badge>{u.passkeys} passkey{u.passkeys === 1 ? '' : 's'}</Badge></>}
              </span>
              {u.role === 'owner' || u.id === meId ? (
                <Badge tone="accent">{u.role}</Badge>
              ) : (
                <select
                  aria-label={`Role of ${u.username}`}
                  value={u.role}
                  onChange={(e) => void changeRole(u, e.target.value as Role)}
                  className="h-7 rounded-md border border-line bg-elevated px-2 text-[12.5px] text-fg outline-none focus:border-accent"
                >
                  {ROLES.map((r) => <option key={r.value} value={r.value}>{r.label}</option>)}
                </select>
              )}
              {SEES_ALL.includes(u.role) ? (
                <span className="hidden text-[12px] text-subtle sm:inline">All storage</span>
              ) : (
                <Button size="sm" variant="ghost" icon={<HardDrive className="size-3.5" />} onClick={() => setAccess(u)} title="Choose what this person can use">
                  {accessLabel(u)}
                </Button>
              )}
              {u.id !== meId && u.two_factor && (
                <Button size="sm" variant="ghost" onClick={() => setResetting(u)} title="For someone who lost their authenticator app">
                  Turn off two-step
                </Button>
              )}
              {u.id !== meId && (u.locked_at === null ? (
                <Button size="sm" variant="ghost" className="hover:text-danger" icon={<Lock className="size-3.5" />} onClick={() => setLocking(u)}>Lock</Button>
              ) : (
                <Button size="sm" variant="ghost" icon={<LockOpen className="size-3.5" />} onClick={() => void change(u, { locked: false }, `${u.username} can sign in again`)}>
                  Unlock
                </Button>
              ))}
              {u.role !== 'owner' && u.id !== meId && (
                <Button size="sm" variant="ghost" className="hover:text-danger" icon={<Trash2 className="size-3.5" />} onClick={() => setRemoving(u)}>
                  Remove
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
      <p className="border-t border-line px-4 py-2.5 text-[12px] text-subtle">
        People sign in with their own name and password in a browser on this computer, with a code or a passkey if they set one up.
        Owners and administrators see all storage; others can be limited to some locations. A locked account cannot sign in. Paired
        devices are managed under Devices.
      </p>

      {access && <PersonAccess user={access} onClose={() => setAccess(null)} onSaved={users.reload} />}

      <Modal
        open={adding}
        onClose={() => setAdding(false)}
        title="Add a person"
        footer={
          <>
            <Button variant="ghost" onClick={() => setAdding(false)}>Cancel</Button>
            <Button variant="primary" loading={busy} disabled={!username.trim() || password.length < 8} onClick={(e) => void add(e)}>Add</Button>
          </>
        }
      >
        <form onSubmit={add} className="space-y-3">
          <Field label="Username" hint="3–32 letters, digits, '.', '_' or '-'">
            <Input value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="off" autoFocus />
          </Field>
          <Field label="Password" hint="At least 8 characters. They can change it after signing in.">
            <Input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" />
          </Field>
          <Field label="Role" hint={ROLES.find((r) => r.value === role)?.can}>
            <select
              value={role}
              onChange={(e) => setRole(e.target.value as Role)}
              className="h-8 w-full rounded-md border border-line bg-bg px-2 text-fg outline-none focus:border-accent"
            >
              {ROLES.map((r) => <option key={r.value} value={r.value}>{r.label}</option>)}
            </select>
          </Field>
          <ErrorNote>{error}</ErrorNote>
          <button type="submit" hidden />
        </form>
      </Modal>

      <Modal
        open={!!locking}
        onClose={() => setLocking(null)}
        title={`Lock ${locking?.username}'s account?`}
        footer={
          <>
            <Button variant="ghost" onClick={() => setLocking(null)}>Cancel</Button>
            <Button variant="danger" onClick={() => locking && void change(locking, { locked: true }, `Locked ${locking.username}'s account`)}>Lock</Button>
          </>
        }
      >
        <p>{locking?.username} is signed out everywhere and cannot sign in, with a password or a passkey, until you unlock the account. Nothing else changes.</p>
      </Modal>

      <Modal
        open={!!resetting}
        onClose={() => setResetting(null)}
        title={`Turn off two-step sign-in for ${resetting?.username}?`}
        footer={
          <>
            <Button variant="ghost" onClick={() => setResetting(null)}>Cancel</Button>
            <Button variant="danger" onClick={() => resetting && void change(resetting, { two_factor: false }, `Two-step sign-in is off for ${resetting.username}`)}>Turn off</Button>
          </>
        }
      >
        <p>
          For someone who lost their phone and recovery codes. {resetting?.username} then signs in with just their password and can turn
          two-step sign-in on again in Settings → Security.
        </p>
      </Modal>

      <Modal
        open={!!removing}
        onClose={() => setRemoving(null)}
        title={`Remove ${removing?.username}?`}
        footer={
          <>
            <Button variant="ghost" onClick={() => setRemoving(null)}>Cancel</Button>
            <Button variant="danger" onClick={() => void remove()}>Remove</Button>
          </>
        }
      >
        <p>{removing?.username} will be signed out everywhere and can no longer sign in. Files are not affected.</p>
      </Modal>
    </Panel>
  )
}

/** Registers the background service to start when you sign in to Windows. */
function NotificationsRow() {
  const [level, setLevel] = useState<NotifyLevel>(notifyLevel())
  const choose = (l: NotifyLevel) => {
    setLevel(l)
    setNotifyLevel(l)
  }
  const options: { value: NotifyLevel; label: string }[] = [
    { value: 'important', label: 'Important' },
    { value: 'all', label: 'Everything' },
    { value: 'off', label: 'Off' },
  ]
  return (
    <Row label="Notifications">
      <div className="inline-flex rounded-md border border-line bg-elevated p-0.5">
        {options.map((o) => (
          <button
            key={o.value}
            onClick={() => choose(o.value)}
            className={cx('h-7 rounded px-3 text-[12.5px]', level === o.value ? 'bg-panel text-fg shadow-sm' : 'text-muted hover:text-fg')}
          >
            {o.label}
          </button>
        ))}
      </div>
      <div className="mt-1 text-[12px] text-subtle">
        {level === 'off'
          ? 'No notifications.'
          : level === 'all'
            ? 'Every finished transfer, devices going offline and coming back, and pairing requests.'
            : 'Failed transfers, finished backups and big transfers, and devices asking to pair.'}
        {' '}Shown while the HearthDrive window is not in front.
      </div>
    </Row>
  )
}

/** Reading text in documents and pictures, and what this computer can do. */
function DocumentsPanel() {
  const caps = useResource(() => libraryApi('').capabilities(), [])
  const [busy, setBusy] = useState(false)
  if (!caps.data) return null
  const c = caps.data
  const toggle = async (on: boolean) => {
    setBusy(true)
    try {
      await libraryApi('').setDocumentSettings(on)
      caps.reload()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Panel title="Documents and media" className="mb-4">
      <div className="divide-y divide-line">
        <Row label="Search inside">
          <div className="text-[13px]">
            Search finds documents by what they say: text files, PDFs, Word, Excel, PowerPoint and OpenDocument files.
            {c.ocr ? ' Scanned PDFs are read with the text recognition built into Windows.' : ' Scanned PDFs need Windows, which reads them with its built-in text recognition.'}
          </div>
          <label className="mt-2 flex items-start gap-2 text-[13px]">
            <input type="checkbox" className="mt-0.5 accent-[var(--nx-accent)]" checked={c.ocr_images} disabled={busy || !c.ocr} onChange={(e) => void toggle(e.target.checked)} />
            <span>
              Read text in pictures too
              <span className="block text-[12px] text-subtle">Photos of receipts, screenshots and scans become searchable. Reading every picture takes a while.</span>
            </span>
          </label>
        </Row>
        <Row label="Photos">
          <div className="text-[13px]">
            {c.faces ? 'Photos with people in them are found with the face finder built into Windows; nothing is downloaded and faces are not told apart.' : 'Finding people in photos needs Windows.'}
          </div>
        </Row>
        <Row label="Videos">
          <div className="text-[13px]">
            {c.transcode
              ? 'Videos browsers cannot play (MKV, AVI, WMV…) are converted while they play, with the ffmpeg found on this computer.'
              : 'Videos browsers cannot play (MKV, AVI, WMV…) can be converted while they play once ffmpeg is installed on this computer and on its PATH. HearthDrive does not download it.'}
          </div>
        </Row>
      </div>
    </Panel>
  )
}

function AutostartRow() {
  const state = useResource(() => api.autostart(), [])
  const [busy, setBusy] = useState(false)
  if (!state.data?.supported) return null
  const toggle = async (enabled: boolean) => {
    setBusy(true)
    try {
      await api.setAutostart(enabled)
      toast(enabled ? 'HearthDrive will start when you sign in to Windows' : 'HearthDrive will no longer start automatically')
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
      state.reload()
    }
  }
  return (
    <Row label="At sign-in">
      <label className="flex items-start gap-2.5">
        <input
          type="checkbox"
          className="mt-0.5 accent-[var(--nx-accent)]"
          checked={state.data.enabled}
          disabled={busy}
          onChange={(e) => void toggle(e.target.checked)}
        />
        <span>
          Start HearthDrive when I sign in to Windows
          <span className="mt-0.5 block text-[12px] text-subtle">
            It starts in the system tray, without this window, so your other devices can reach this computer after a
            restart and you get notifications.
          </span>
        </span>
      </label>
    </Row>
  )
}

/** What closing the window does: stay in the tray, or quit the app. */
function CloseRow() {
  const state = useResource(() => closeAction(), [])
  const [busy, setBusy] = useState(false)
  if (!state.data) return null
  const choose = async (action: CloseAction) => {
    setBusy(true)
    try {
      await setCloseAction(action)
      state.reload()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  const options: { value: CloseAction; label: string; hint: string }[] = [
    { value: 'tray', label: 'Keep running in the system tray', hint: 'Notifications keep coming. Open it again from the tray icon.' },
    { value: 'quit', label: 'Quit the app', hint: 'No tray icon or notifications until you open HearthDrive again.' },
  ]
  return (
    <Row label="Closing the window">
      <div className="space-y-2">
        {options.map((o) => (
          <label key={o.value} className="flex items-start gap-2.5">
            <input
              type="radio"
              name="close-action"
              className="mt-0.5 accent-[var(--nx-accent)]"
              checked={state.data === o.value}
              disabled={busy}
              onChange={() => void choose(o.value)}
            />
            <span>
              {o.label}
              <span className="mt-0.5 block text-[12px] text-subtle">{o.hint}</span>
            </span>
          </label>
        ))}
      </div>
    </Row>
  )
}

/** Keeping the computer awake while files move (a transfer setting). */
function AwakeRow() {
  const settings = useResource(() => api.transferSettings(), [])
  const [busy, setBusy] = useState(false)
  const data = settings.data
  if (!data) return null
  const toggle = async (keep_awake: boolean) => {
    setBusy(true)
    try {
      const { can_configure: _, ...current } = data
      await api.setTransferSettings({ ...current, keep_awake })
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
      settings.reload()
    }
  }
  return (
    <Row label="Sleep">
      <label className="flex items-start gap-2.5">
        <input
          type="checkbox"
          className="mt-0.5 accent-[var(--nx-accent)]"
          checked={data.keep_awake}
          disabled={busy || !data.can_configure}
          onChange={(e) => void toggle(e.target.checked)}
        />
        <span>
          Keep this computer awake while files are moving
          <span className="mt-0.5 block text-[12px] text-subtle">
            Windows does not go to sleep while transfers run here or another device is copying to or from this computer.
            The screen can still turn off.
          </span>
        </span>
      </label>
    </Row>
  )
}

/** Everything about HearthDrive running while no window is open. */
function BackgroundPanel() {
  return (
    <Panel title="Running in the background" className="mb-4">
      <div className="divide-y divide-line">
        <Row label="Storage service">
          Always running while Windows is on, even with the app closed, so your other devices can reach this computer.
          While files are moving, HearthDrive asks Windows 11 not to slow it down with Efficiency mode.
        </Row>
        <AutostartRow />
        <CloseRow />
        <AwakeRow />
      </div>
    </Panel>
  )
}

async function pickFolder(): Promise<string | null> {
  if (!isTauri()) return null
  const { open } = await import('@tauri-apps/plugin-dialog')
  const picked = await open({ directory: true, multiple: false, title: 'Choose an empty folder for the virtual drive' })
  return typeof picked === 'string' ? picked : null
}

/** The virtual drive's offline cache: size limit and clearing. */
function CacheRow({ data, onSaved }: { data: VirtualDriveStatus; onSaved: () => void }) {
  const [limit, setLimit] = useState(String(data.cache.limit_gb))
  const [days, setDays] = useState(String(data.cache.clear_after_days))
  const [frequent, setFrequent] = useState(data.cache.keep_frequent)
  const [busy, setBusy] = useState(false)
  const changed =
    Number(limit) !== data.cache.limit_gb || Number(days) !== data.cache.clear_after_days || frequent !== data.cache.keep_frequent
  const save = async () => {
    setBusy(true)
    try {
      await api.setVirtualDriveCache({ limit_gb: Number(limit) || 0, clear_after_days: Math.round(Number(days) || 0), keep_frequent: frequent })
      toast('Offline cache settings saved')
      onSaved()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  const s = data.cache_status
  return (
    <Row label="Offline cache">
      <div className="flex flex-wrap items-center gap-2 text-[13px]">
        Keep up to
        <Input className="w-20" inputMode="decimal" value={limit} onChange={(e) => setLimit(e.target.value)} />
        GB of downloaded files; clear files not opened for
        <Input className="w-16" inputMode="numeric" value={days} onChange={(e) => setDays(e.target.value)} />
        days
      </div>
      <label className="mt-2 flex items-center gap-2 text-[13px]">
        <input type="checkbox" className="accent-[var(--nx-accent)]" checked={frequent} onChange={(e) => setFrequent(e.target.checked)} />
        Keep often used files downloaded, and download them again when they change
      </label>
      <div className="mt-1 text-[12px] text-subtle">
        {data.running && s.checked_at
          ? `${formatBytes(s.used_bytes)} in ${s.files.toLocaleString()} downloaded file${s.files === 1 ? '' : 's'}` +
            (s.kept_bytes ? `, plus ${formatBytes(s.kept_bytes)} kept on this device` : '') +
            (s.frequent ? ` · ${s.frequent} often used` : '') +
            (s.cleared ? ` · ${s.cleared} cleared so far` : '')
          : 'The least recently used files are cleared first. 0 means no limit, or never.'}
        {' '}Files kept with Always keep on this device and files changed here are never cleared.
      </div>
      {changed && (
        <Button className="mt-2" size="sm" variant="primary" loading={busy} onClick={save}>Save</Button>
      )}
    </Row>
  )
}

/** Paired devices' storage in File Explorer, as a cloud drive (Windows). */
function VirtualDrivePanel() {
  const state = useResource(() => api.virtualDrive(), [])
  const [busy, setBusy] = useState(false)
  const [folder, setFolder] = useState<string | null>(null)
  const data = state.data
  if (!data?.supported) return null
  const chosen = (folder ?? data.path).trim()
  const moved = data.enabled && chosen !== data.path

  const apply = async (enabled: boolean, path: string) => {
    setBusy(true)
    try {
      const s = await api.setVirtualDrive({ enabled, path: path === data.default_path ? '' : path })
      setFolder(null)
      toast(
        !enabled
          ? 'The virtual drive was removed from File Explorer'
          : moved
            ? `The virtual drive moved to ${s.path}`
            : `Your devices are in File Explorer under HearthDrive (${s.path})`,
      )
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
      state.reload()
    }
  }

  return (
    <Panel title="Virtual drive" className="mb-4">
      <div className="divide-y divide-line">
        <Row label="File Explorer">
          <label className="flex items-start gap-2.5">
            <input
              type="checkbox"
              className="mt-0.5 accent-[var(--nx-accent)]"
              checked={data.enabled}
              disabled={busy}
              onChange={(e) => void apply(e.target.checked, chosen)}
            />
            <span>
              Show my devices in File Explorer
              <span className="mt-0.5 block text-[12px] text-subtle">
                HearthDrive in File Explorer has My Files (this computer and every paired device) and Shared With Me (folders
                other devices shared with this one). Files download when you open them; right-click and choose Always
                keep on this device, or Free up space. What you change there goes back to the device: edits and new
                files are uploaded once saved, and renames, moves and deletes happen on the device (deleted items go to
                its trash). Changes on other devices show up within seconds.
              </span>
            </span>
          </label>
          {data.enabled && (
            <div className="mt-2">
              {busy ? (
                <Spinner />
              ) : data.running ? (
                <Badge tone="accent">Running</Badge>
              ) : (
                <Badge tone="warn">Not running</Badge>
              )}
            </div>
          )}
          {data.error && <div className="mt-2"><ErrorNote>{data.error}</ErrorNote></div>}
        </Row>
        <CacheRow data={data} onSaved={() => state.reload()} />
        <Row label="Folder">
          <div className="flex gap-2">
            <Input
              value={chosen}
              disabled={busy}
              spellCheck={false}
              onChange={(e) => setFolder(e.target.value)}
              className="font-mono text-[12px]"
            />
            {isTauri() && (
              <Button
                size="sm"
                icon={<FolderOpen className="size-3.5" />}
                disabled={busy}
                onClick={() => void pickFolder().then((p) => p && setFolder(p))}
              >
                Browse
              </Button>
            )}
            {moved && (
              <Button size="sm" variant="primary" loading={busy} onClick={() => void apply(true, chosen)}>
                Move
              </Button>
            )}
          </div>
          <div className="mt-1 text-[12px] text-subtle">
            An empty or new folder on this computer. Turning the drive off or moving it deletes the downloaded copies
            in it; files on your devices are never touched.
          </div>
        </Row>
      </div>
    </Panel>
  )
}

function TerminalPanel() {
  const info = useResource(() => terminalApi('').info(), [])
  const [busy, setBusy] = useState(false)
  if (!info.data) return null
  const here = isMobile() ? 'phone' : 'computer'
  const toggle = async (on: boolean) => {
    setBusy(true)
    try {
      await setTerminalEnabled(on)
      info.reload()
      toast(on ? `Your other devices can open a terminal on this ${here}` : `Terminals from other devices are closed and turned off`)
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Panel title="Terminal" className="mb-4">
      <div className="divide-y divide-line">
        <Row label="From other devices">
          <label className="flex items-start gap-2 text-[13px]">
            <input type="checkbox" className="mt-0.5 accent-[var(--nx-accent)]" checked={info.data.enabled} disabled={busy} onChange={(e) => void toggle(e.target.checked)} />
            <span>
              Let my other devices open a terminal on this {here}
              <span className="block text-[12px] text-subtle">
                Open it from Devices › Terminal on another computer or your phone. Only you can use it, and only from a device with Editor access or higher here.
                It runs {info.data.shell} with your rights on this {here}, so it can do anything you can: turn it off if you don't use it.
              </span>
            </span>
          </label>
        </Row>
        {isMobile() && info.data.ssh && <TermuxRow ssh={info.data.ssh} onChanged={info.reload} />}
      </div>
    </Panel>
  )
}

/** The command that lets HearthDrive into Termux, and starts its SSH server. */
function termuxSetup(publicKey: string) {
  return [
    'pkg install -y openssh',
    'mkdir -p ~/.ssh',
    `grep -qxF '${publicKey}' ~/.ssh/authorized_keys 2>/dev/null || echo '${publicKey}' >> ~/.ssh/authorized_keys`,
    'chmod 600 ~/.ssh/authorized_keys',
    "grep -q 'pgrep -x sshd' ~/.bashrc 2>/dev/null || echo 'pgrep -x sshd >/dev/null || sshd' >> ~/.bashrc",
    'sshd',
  ].join(' && ')
}

/** Termux instead of Android's basic shell (phones). */
function TermuxRow({ ssh, onChanged }: { ssh: NonNullable<TerminalInfo['ssh']>; onChanged: () => void }) {
  const [busy, setBusy] = useState(false)
  const [checking, setChecking] = useState(false)
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null)
  const [port, setPort] = useState(String(ssh.port))
  const [user, setUser] = useState(ssh.user)
  const command = termuxSetup(ssh.public_key)

  const change = async (c: Parameters<typeof setTerminalShell>[0]) => {
    setBusy(true)
    setResult(null)
    try {
      await setTerminalShell(c)
      onChanged()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  const check = async () => {
    setChecking(true)
    setResult(null)
    try {
      const r = await testTerminalShell()
      setResult({ ok: true, text: r.message })
      onChanged()
    } catch (e) {
      setResult({ ok: false, text: e instanceof Error ? e.message : String(e) })
    } finally {
      setChecking(false)
    }
  }
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command)
      toast('Copied. Paste it in Termux and press Enter')
    } catch {
      toast('Select the command and copy it')
    }
  }

  return (
    <Row label="Shell">
      <div className="space-y-2 text-[13px]">
        {([false, true] as const).map((on) => (
          <label key={String(on)} className="flex items-start gap-2">
            <input type="radio" name="terminal-shell" className="mt-0.5 accent-[var(--nx-accent)]" checked={ssh.on === on} disabled={busy} onChange={() => void change({ on })} />
            <span>
              {on ? 'Termux' : 'Android’s basic shell'}
              <span className="block text-[12px] text-subtle">
                {on
                  ? 'A full Linux shell with packages (pkg, python, git…). HearthDrive signs in to Termux on this phone with its own key.'
                  : 'Built in, and only reaches HearthDrive’s own folders and a few commands.'}
              </span>
            </span>
          </label>
        ))}
      </div>
      {ssh.on && (
        <div className="mt-3 space-y-3 rounded-md border border-line bg-elevated p-3 text-[12.5px]">
          <ol className="list-decimal space-y-2 pl-4">
            <li>Install Termux from F-Droid or its GitHub page (the Play Store version is out of date) and open it.</li>
            <li>
              Paste this in Termux and press Enter. It lets HearthDrive in and starts Termux’s SSH server, also each time you open Termux from now on.
              <pre className="mt-1.5 max-h-28 overflow-auto rounded border border-line bg-panel p-2 font-mono text-[11px] leading-snug whitespace-pre-wrap [overflow-wrap:anywhere] select-all">{command}</pre>
              <Button size="sm" className="mt-1.5" onClick={() => void copy()}>Copy command</Button>
            </li>
            <li>
              Come back and check the connection. After the phone restarts, open Termux once so its SSH server runs again.
              <div className="mt-1.5 flex flex-wrap items-center gap-2">
                <Button size="sm" loading={checking} onClick={() => void check()}>Check connection</Button>
                {ssh.trusted && !result && <span className="text-subtle">Connected before</span>}
              </div>
              {result && <div className={cx('mt-1.5', result.ok ? 'text-success' : 'text-danger')}>{result.text}</div>}
            </li>
          </ol>
          <details>
            <summary className="cursor-pointer text-muted">Other SSH server</summary>
            <div className="mt-2 space-y-2">
              <p className="text-subtle">
                Termux’s SSH server uses port 8022 and any user name. For Android’s Linux terminal, run an SSH server in it, forward its port to the phone, and enter the port and user name here.
              </p>
              <div className="flex flex-wrap items-end gap-2">
                <Field label="Port">
                  <Input className="w-24" inputMode="numeric" value={port} onChange={(e) => setPort(e.target.value.replace(/\D/g, ''))} />
                </Field>
                <Field label="User">
                  <Input className="w-36" autoCapitalize="off" value={user} onChange={(e) => setUser(e.target.value)} />
                </Field>
                <Button size="sm" disabled={busy || (port === String(ssh.port) && user === ssh.user)} onClick={() => void change({ port: Number(port), user })}>Save</Button>
              </div>
              {ssh.trusted && (
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => void change({ forget_host: true })}>
                  Forget the server
                </Button>
              )}
              <p className="text-subtle">Forget the server after reinstalling Termux, when HearthDrive says the server’s key has changed.</p>
            </div>
          </details>
        </div>
      )}
    </Row>
  )
}

export function Settings() {
  const { phase, signOut } = useSession()
  const [theme, setTheme] = useTheme()
  const info = useResource(() => api.systemInfo(), [])
  const me = phase.kind === 'ready' ? phase.me : null

  const themes: { value: ThemePref; label: string; icon: typeof Sun }[] = [
    { value: 'dark', label: 'Dark', icon: Moon },
    { value: 'light', label: 'Light', icon: Sun },
    { value: 'system', label: 'System', icon: Monitor },
  ]

  return (
    <div className="max-w-3xl">
      <PageHeader title="Settings" />

      <Panel title="General" className="mb-4">
        <div className="divide-y divide-line">
          <Row label="Theme">
            <div className="inline-flex rounded-md border border-line bg-elevated p-0.5">
              {themes.map(({ value, label, icon: Icon }) => (
                <button
                  key={value}
                  onClick={() => setTheme(value)}
                  className={cx(
                    'flex h-7 items-center gap-1.5 rounded px-3 text-[12.5px]',
                    theme === value ? 'bg-panel text-fg shadow-sm' : 'text-muted hover:text-fg',
                  )}
                >
                  <Icon className="size-3.5" /> {label}
                </button>
              ))}
            </div>
          </Row>
          <NotificationsRow />
        </div>
      </Panel>

      {!isMobile() && isTauri() && <BackgroundPanel />}

      <Panel title="Account" className="mb-4">
        <div className="divide-y divide-line">
          <Row label="Signed in as">
            <span className="font-medium">{me?.username}</span> <Badge tone="accent">{me?.role}</Badge>
          </Row>
          <Row label="Session">
            {me?.local ? (
              isMobile() ? 'This app signs in to its own node automatically. There is no password to remember.' : 'Desktop app — authenticated automatically on this computer'
            ) : (
              <Button size="sm" icon={<LogOut className="size-3.5" />} onClick={() => void signOut()}>Sign out</Button>
            )}
          </Row>
        </div>
      </Panel>

      {!isMobile() && me?.user_id && !me.device_id && <SecurityPanel />}

      {!isMobile() && me && (me.role === 'owner' || me.role === 'administrator') && <People meId={me.user_id} />}

      {isMobile() && me && me.role === 'owner' && (
        <Panel title="Phone storage" className="mb-4">
          <PhoneStorageSettings />
        </Panel>
      )}

      <Panel title="Service" className="mb-4">
        {info.data ? (
          <div className="divide-y divide-line">
            <Row label="Node">{info.data.node.name} <span className="text-subtle">({info.data.node.hostname})</span></Row>
            <Row label="Status">Running · up {formatDuration(info.data.uptime_ms)}</Row>
            <Row label="API">
              <span className="font-mono text-[12px]">{connection().baseUrl}/api/v1</span>
            </Row>
            <Row label="Network">{info.data.network.status.display_name} — {info.data.network.status.detail}</Row>
            <Row label="Data folder">
              <span className="font-mono text-[12px]">{info.data.data_dir}</span>
              <div className="mt-1 text-[12px] text-subtle">Database, logs and runtime state. Your files are never stored here.</div>
            </Row>
            {isMobile() && (
              <Row label="While the app is closed">
                HearthDrive runs on this phone while the app is open. Your computers keep running on their own and are reachable again as soon as you open the app.
              </Row>
            )}
          </div>
        ) : (
          <Spinner />
        )}
      </Panel>

      {!isMobile() && me && (me.role === 'owner' || me.role === 'administrator') && <VirtualDrivePanel />}
      {me && (me.role === 'owner' || me.role === 'administrator') && <DocumentsPanel />}
      {me && me.role === 'owner' && !me.device_id && <TerminalPanel />}
      {!isMobile() && me && me.user_id && <ProgramsPanel manage={me.role === 'owner' || me.role === 'administrator'} />}

      <Panel title="Updates" className="mb-4">
        <UpdateSettings />
      </Panel>

      <Panel title="About">
        <div className="divide-y divide-line">
          <Row label="Version">HearthDrive {info.data?.node.version ?? ''}</Row>
          <Row label="License">Apache 2.0 · free and open source, self-hosted, no subscription, no analytics</Row>
        </div>
      </Panel>
    </div>
  )
}
