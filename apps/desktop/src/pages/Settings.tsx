import { useCallback, useState } from 'react'
import { FolderOpen, HardDrive, LogOut, Monitor, Moon, Sun, Trash2, UserPlus } from 'lucide-react'
import { api, connection, fileApi, isMobile, isTauri, peerPrefix, thisDevice, type Device, type Role, type StorageRoot, type User } from '../services/api'
import { toast, toastError } from '../components/Toasts'
import { useResource } from '../hooks/useResource'
import { useSession } from '../stores/session'
import { useTheme, type ThemePref } from '../stores/theme'
import { formatDuration } from '../lib/format'
import { UpdateSettings } from '../components/Updates'
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

/** Accounts that can sign in to this computer's NEXUS from a browser on it. */
function People({ meId }: { meId: string | null }) {
  const users = useResource(() => api.users(), [])
  const [adding, setAdding] = useState(false)
  const [removing, setRemoving] = useState<User | null>(null)
  const [access, setAccess] = useState<User | null>(null)
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
        People sign in with their own name and password in a browser on this computer. Owners and administrators see all storage;
        others can be limited to some locations. Paired devices are managed under Devices.
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
function AutostartRow() {
  const state = useResource(() => api.autostart(), [])
  const [busy, setBusy] = useState(false)
  if (!state.data?.supported) return null
  const toggle = async (enabled: boolean) => {
    setBusy(true)
    try {
      await api.setAutostart(enabled)
      toast(enabled ? 'NEXUS will start when you sign in to Windows' : 'NEXUS will no longer start automatically')
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
      state.reload()
    }
  }
  return (
    <Row label="Start with Windows">
      <label className="flex items-start gap-2.5">
        <input
          type="checkbox"
          className="mt-0.5 accent-[var(--nx-accent)]"
          checked={state.data.enabled}
          disabled={busy}
          onChange={(e) => void toggle(e.target.checked)}
        />
        <span>
          Start the storage service when I sign in
          <span className="mt-0.5 block text-[12px] text-subtle">
            Keeps this computer reachable from your other devices after a restart, without opening this window.
          </span>
        </span>
      </label>
    </Row>
  )
}

async function pickFolder(): Promise<string | null> {
  if (!isTauri()) return null
  const { open } = await import('@tauri-apps/plugin-dialog')
  const picked = await open({ directory: true, multiple: false, title: 'Choose an empty folder for the virtual drive' })
  return typeof picked === 'string' ? picked : null
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
            : `Your devices are in File Explorer under NEXUS (${s.path})`,
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
                Every paired device's storage appears under NEXUS in File Explorer. Files download when you open them;
                right-click a file or folder and choose Always keep on this device, or Free up space. Read-only for now:
                changes made there are not sent back to the device.
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
        </div>
      </Panel>

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

      {!isMobile() && me && (me.role === 'owner' || me.role === 'administrator') && <People meId={me.user_id} />}

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
            {!isMobile() && <AutostartRow />}
            <Row label={isMobile() ? 'While the app is closed' : 'Background service'}>
              {isMobile()
                ? 'NEXUS runs on this phone while the app is open. Your computers keep running on their own and are reachable again as soon as you open the app.'
                : 'The storage service keeps running when this window is closed.'}
            </Row>
          </div>
        ) : (
          <Spinner />
        )}
      </Panel>

      {!isMobile() && me && (me.role === 'owner' || me.role === 'administrator') && <VirtualDrivePanel />}

      <Panel title="Updates" className="mb-4">
        <UpdateSettings />
      </Panel>

      <Panel title="About">
        <div className="divide-y divide-line">
          <Row label="Version">NEXUS Cloud {info.data?.node.version ?? ''}</Row>
          <Row label="License">MIT · free, self-hosted, no subscription, no analytics</Row>
        </div>
      </Panel>
    </div>
  )
}
