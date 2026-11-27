import { useEffect, useState } from 'react'
import {
  ArrowDown, ArrowUp, Ban, FolderOpen, Laptop, Link2, Monitor, Network, Power, QrCode, TerminalSquare, Zap, RefreshCw, ShieldCheck, Smartphone, Trash2, Wifi,
} from 'lucide-react'
import { api, isMobile, thisDevice, type Device, type NearbyDevice, type NetworkInfo, type Role, type StorageRoot } from '../services/api'
import { useResource } from '../hooks/useResource'
import { navigate } from '../hooks/useRoute'
import { formatBytes, formatRelative } from '../lib/format'
import { FingerprintText, InviteModal, JoinModal, RequestCard, describeHint, routeParts } from '../components/Pairing'
import { toast, toastError } from '../components/Toasts'
import { Badge, Button, ErrorNote, Modal, PageHeader, Panel, Spinner, StatusDot } from '../components/ui'

const DEVICE_ROLES: { value: Role; label: string }[] = [
  { value: 'administrator', label: 'Administrator' },
  { value: 'editor', label: 'Editor' },
  { value: 'viewer', label: 'Viewer' },
]

function deviceIcon(type: string) {
  return type === 'android' || type === 'ios' ? Smartphone : type === 'laptop' ? Laptop : Monitor
}

const RELAY_MODE: Record<NetworkInfo['status']['relay_mode'], string> = {
  default: 'Public relays (iroh)',
  custom: 'Your own relay',
  disabled: 'Off — direct only',
}

function stateTone(n: NetworkInfo): 'online' | 'offline' | 'busy' | 'error' {
  switch (n.status.state) {
    case 'online': return 'online'
    case 'starting': return 'busy'
    case 'local_only': return 'busy'
    default: return 'offline'
  }
}

const STATE_LABEL: Record<NetworkInfo['status']['state'], string> = {
  online: 'Reachable from anywhere',
  starting: 'Connecting…',
  local_only: 'Local network only',
  off: 'Remote access off',
}

function NetworkPanel() {
  const net = useResource(() => api.network(), [], (e) => e.type === 'network.changed')
  const [busy, setBusy] = useState<'refresh' | 'toggle' | null>(null)
  const n = net.data

  const run = async (kind: 'refresh' | 'toggle', fn: () => Promise<unknown>) => {
    setBusy(kind)
    try {
      await fn()
      net.reload()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(null)
    }
  }

  return (
    <Panel
      title="Connections"
      actions={
        <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} loading={busy === 'refresh'} onClick={() => void run('refresh', api.refreshNetwork)}>
          Check again
        </Button>
      }
    >
      {!n ? (
        <Spinner />
      ) : (
        <div className="grid grid-cols-1 gap-6 md:grid-cols-5">
          <div className="space-y-3 md:col-span-3">
            <div className="flex items-center gap-2">
              <StatusDot tone={stateTone(n)} />
              <span className="font-medium">{STATE_LABEL[n.status.state]}</span>
              <Badge>{n.status.display_name}</Badge>
            </div>
            <p className="text-muted">{n.status.detail}</p>
            <dl className="grid grid-cols-2 gap-3 text-[12px]">
              <div>
                <dt className="text-subtle">Relay</dt>
                <dd className="mt-0.5 truncate font-mono">
                  {n.status.relay_url ? describeHint(`relay:${n.status.relay_url}`).replace(/^relay /, '') : '—'}
                </dd>
                <dd className="text-subtle">{RELAY_MODE[n.status.relay_mode]}</dd>
              </div>
              <div>
                <dt className="text-subtle">Direct addresses</dt>
                <dd className="mt-0.5 font-mono">
                  {n.status.direct_addresses.length
                    ? n.status.direct_addresses.slice(0, 3).map((a) => <div key={a} className="truncate">{a}</div>)
                    : '—'}
                </dd>
              </div>
            </dl>
            {n.status.state === 'local_only' && n.status.relay_error && <ErrorNote>{n.status.relay_error}</ErrorNote>}
            <label className="flex items-start gap-3 rounded-md border border-line bg-elevated p-3">
              <input
                type="checkbox"
                className="mt-0.5 accent-[var(--nx-accent)]"
                checked={n.remote_enabled}
                disabled={busy === 'toggle'}
                onChange={(e) => void run('toggle', () => api.setRemoteAccess(e.target.checked))}
              />
              <span>
                <span className="font-medium">Allow access from my devices</span>
                <span className="mt-0.5 block text-[12px] text-muted">
                  {n.remote_enabled
                    ? 'Only devices you paired can connect, and only to browse and transfer files. Nothing is opened to the internet.'
                    : `Off: paired devices cannot connect to this ${thisDevice()} and see it as offline. You can still browse them from here.`}
                </span>
              </span>
            </label>
          </div>
          <div className="space-y-4 md:col-span-2 md:border-l md:border-line md:pl-6">
            <div>
              <div className="text-[12px] text-subtle">This {thisDevice()}'s fingerprint</div>
              <FingerprintText value={n.fingerprint} className="mt-1 block" />
              <p className="mt-1 text-[12px] text-subtle">Compare it when pairing. The key behind it never leaves this {thisDevice()}{isMobile() ? ' and is kept in the app\'s private storage' : ' and is protected by Windows'}.</p>
            </div>
            <div className="text-[12px] text-subtle">
              <div className="flex items-center gap-1.5 font-medium text-muted"><ShieldCheck className="size-3.5" /> How devices connect</div>
              <p className="mt-1">
                Connections are end-to-end encrypted and tied to each device's key. They go directly between your computers whenever possible;
                otherwise a relay passes along the encrypted traffic without being able to read it.
              </p>
            </div>
          </div>
        </div>
      )}
    </Panel>
  )
}

/** Chooses which of this device's storage locations a paired device may see. */
function AccessModal({ device, onClose }: { device: Device; onClose: () => void }) {
  const roots = useResource(() => api.roots(), [])
  const [all, setAll] = useState(device.allowed_roots === null)
  const [chosen, setChosen] = useState<Set<string>>(new Set(device.allowed_roots ?? []))
  const [busy, setBusy] = useState(false)
  const list: StorageRoot[] = roots.data ?? []

  const save = async () => {
    setBusy(true)
    try {
      await api.updateDevice(device.id, all ? { all_roots: true } : { allowed_roots: [...chosen] })
      toast(all ? `${device.name} can see all storage` : `${device.name} can see ${chosen.size} of ${list.length} locations`)
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
      title={`What ${device.name} can see`}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} onClick={() => void save()}>Save</Button>
        </>
      }
    >
      <div className="space-y-3">
        <p className="text-muted">
          Choose which storage on this {thisDevice()} {device.name} can browse, search and change. Everything else stays invisible to it.
        </p>
        <label className="flex items-center gap-2.5">
          <input type="radio" name="access" className="accent-[var(--nx-accent)]" checked={all} onChange={() => setAll(true)} />
          <span>All storage, including locations added later</span>
        </label>
        <label className="flex items-center gap-2.5">
          <input type="radio" name="access" className="accent-[var(--nx-accent)]" checked={!all} onChange={() => setAll(false)} />
          <span>Only these locations</span>
        </label>
        <div className={all ? 'pointer-events-none opacity-50' : ''}>
          {roots.loading && !roots.data ? (
            <Spinner />
          ) : list.length === 0 ? (
            <p className="text-muted">This {thisDevice()} has no storage yet.</p>
          ) : (
            <ul className="divide-y divide-line rounded-md border border-line">
              {list.map((r) => (
                <li key={r.id}>
                  <label className="flex items-center gap-2.5 px-3 py-2">
                    <input
                      type="checkbox"
                      className="accent-[var(--nx-accent)]"
                      disabled={all}
                      checked={chosen.has(r.id)}
                      onChange={(e) =>
                        setChosen((prev) => {
                          const next = new Set(prev)
                          if (e.target.checked) next.add(r.id)
                          else next.delete(r.id)
                          return next
                        })
                      }
                    />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate font-medium">{r.name}</span>
                      <span className="block truncate text-[12px] text-subtle">{r.absolute_path}</span>
                    </span>
                  </label>
                </li>
              ))}
            </ul>
          )}
        </div>
        {!all && chosen.size === 0 && list.length > 0 && (
          <p className="text-[12px] text-warn">With nothing selected, {device.name} will not see any files here.</p>
        )}
      </div>
    </Modal>
  )
}

/** Whether this node introduces its paired devices to each other. */
function IntroductionsToggle() {
  const state = useResource(() => api.introductions(), [])
  const [busy, setBusy] = useState(false)
  if (!state.data) return null
  const toggle = async (enabled: boolean) => {
    setBusy(true)
    try {
      await api.setIntroductions(enabled)
      toast(enabled ? 'Your devices will be paired with each other' : 'Devices are no longer introduced to each other')
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
      state.reload()
    }
  }
  return (
    <label className="mt-4 flex items-start gap-2.5 rounded-lg border border-line bg-panel p-4">
      <input
        type="checkbox"
        className="mt-0.5 accent-[var(--nx-accent)]"
        checked={state.data.enabled}
        disabled={busy}
        onChange={(e) => void toggle(e.target.checked)}
      />
      <span>
        <span className="font-medium">Pair my devices with each other</span>
        <span className="mt-0.5 block text-[12px] text-subtle">
          Every device paired with this {thisDevice()} is introduced to the others, so they pair without scanning a code. An introduced device gets no
          more access than this {thisDevice()} has there, and is removed everywhere when you remove it here.
        </span>
      </span>
    </label>
  )
}

function DeviceCard({ d, introducer, onRemove, onBlock }: {
  d: Device
  introducer: string | null
  onRemove: (d: Device) => void
  onBlock: (d: Device, blocked: boolean) => void
}) {
  const Icon = deviceIcon(d.device_type)
  const [role, setRole] = useState<Role>(d.role)
  const [editingAccess, setEditingAccess] = useState(false)
  const changeRole = async (r: Role) => {
    setRole(r)
    try {
      await api.updateDevice(d.id, { role: r })
      toast(`${d.name} is now ${r === 'administrator' ? 'an' : 'a'} ${r}`)
    } catch (e) {
      setRole(d.role)
      toastError(e)
    }
  }
  return (
    <div className="flex flex-col rounded-lg border border-line bg-panel p-4">
      <div className="flex items-start gap-3">
        <div className="rounded-md bg-elevated p-2"><Icon className="size-5 text-accent" /></div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate font-medium">{d.name}</span>
            {d.is_local && <Badge tone="accent">This {thisDevice()}</Badge>}
            {d.blocked_at !== null && <Badge tone="danger">Blocked</Badge>}
          </div>
          {d.introduced_by && <div className="mt-0.5 truncate text-[12px] text-subtle">Introduced by {introducer ?? 'a paired device'}</div>}
          <div className="mt-0.5 flex items-center gap-1.5 text-[12px] text-muted">
            <StatusDot tone={d.blocked_at !== null ? 'error' : d.online ? 'online' : 'offline'} />
            {d.blocked_at !== null ? `Blocked ${formatRelative(d.blocked_at)}: nothing goes to or from it` : d.is_local ? 'Online' : d.online ? `Online${d.latency_ms != null ? ` · ${d.latency_ms} ms` : ''}` : d.last_seen ? `Offline · seen ${formatRelative(d.last_seen)}` : 'Offline · not seen yet'}
          </div>
          {!d.is_local && d.online && (d.rx_bps > 0 || d.tx_bps > 0) && (
            <div className="mt-0.5 flex items-center gap-2 text-[12px] tabular text-muted" title="Traffic with this device right now">
              <span className="flex items-center gap-0.5"><ArrowDown className="size-3" />{formatBytes(d.rx_bps)}/s</span>
              <span className="flex items-center gap-0.5"><ArrowUp className="size-3" />{formatBytes(d.tx_bps)}/s</span>
            </div>
          )}
        </div>
      </div>
      {!d.is_local && !d.online && d.blocked_at === null && d.unreachable && (
        <div className="mt-3 rounded-md bg-elevated px-3 py-2 text-[12px] text-muted" title={d.unreachable.detail}>
          {d.unreachable.reason}
          <div className="mt-0.5 text-subtle">Unreachable since {formatRelative(d.unreachable.since)}</div>
        </div>
      )}
      <dl className="mt-4 grid flex-1 grid-cols-2 content-start gap-3 text-[12px]">
        {d.is_local ? (
          <>
            <div><dt className="text-subtle">Storage locations</dt><dd className="mt-0.5">{d.roots}</dd></div>
            <div><dt className="text-subtle">Indexed</dt><dd className="mt-0.5 tabular">{formatBytes(d.indexed_bytes)}</dd></div>
          </>
        ) : (
          <>
            <div><dt className="text-subtle">Paired</dt><dd className="mt-0.5">{formatRelative(d.paired_at)}</dd></div>
            <div>
              <dt className="text-subtle">Access to this {thisDevice()}</dt>
              <dd className="mt-0.5">
                <select
                  value={role}
                  onChange={(e) => void changeRole(e.target.value as Role)}
                  className="-ml-1 rounded border border-transparent bg-transparent px-0.5 text-fg outline-none hover:border-line focus:border-accent"
                >
                  {DEVICE_ROLES.map((r) => <option key={r.value} value={r.value}>{r.label}</option>)}
                </select>
              </dd>
            </div>
          </>
        )}
        {!d.is_local && (
          <div className="col-span-2">
            <dt className="text-subtle">Storage it can see here</dt>
            <dd className="mt-0.5">
              <button className="text-fg underline decoration-line-strong underline-offset-2 hover:decoration-accent" onClick={() => setEditingAccess(true)}>
                {d.allowed_roots === null ? 'All storage' : d.allowed_roots.length === 0 ? 'Nothing' : `${d.allowed_roots.length} location${d.allowed_roots.length === 1 ? '' : 's'}`}
              </button>
            </dd>
          </div>
        )}
        <div className="col-span-2">
          <dt className="text-subtle">Fingerprint</dt>
          <dd className="mt-0.5"><FingerprintText value={d.fingerprint} className="text-[11px]" /></dd>
        </div>
        {d.is_local ? (
          d.addresses.length > 0 && (
            <div className="col-span-2">
              <dt className="text-subtle">Reachable through</dt>
              <dd className="mt-0.5 truncate font-mono text-[11.5px]">{d.addresses.map(describeHint).join(' · ')}</dd>
            </div>
          )
        ) : (
          (d.route ?? d.network_address) && (
            <div className="col-span-2">
              <dt className="text-subtle">{d.route ? 'Connection' : 'Last connection'}</dt>
              {(() => {
                const r = routeParts((d.route ?? d.network_address)!)
                return (
                  <dd className="mt-0.5 truncate text-[12px]" title={`${r.kind} · ${r.where}`}>
                    <span className="font-medium">{r.kind}</span> <span className="font-mono text-[11.5px] text-muted">· {r.where}</span>
                  </dd>
                )
              })()}
            </div>
          )
        )}
      </dl>
      {!d.is_local && (
        // Wraps onto a second line in a narrow card instead of spilling out;
        // the actions that cut the device off stay together on the right.
        <div className="mt-4 flex flex-wrap items-center gap-2 border-t border-line pt-3">
          {d.blocked_at === null ? (
            <>
              <Button size="sm" icon={<FolderOpen className="size-3.5" />} onClick={() => navigate('files', { device: d.id })}>Browse files</Button>
              {d.online && <CommandsButton d={d} />}
              {d.online && (
                <Button size="sm" variant="ghost" icon={<TerminalSquare className="size-3.5" />} onClick={() => navigate('terminal', { device: d.id })}>Terminal</Button>
              )}
              {!d.online && d.can_wake && <WakeButton d={d} />}
            </>
          ) : (
            <Button size="sm" icon={<Ban className="size-3.5" />} onClick={() => onBlock(d, false)}>Unblock</Button>
          )}
          <div className="ml-auto flex items-center gap-1">
            {d.blocked_at === null && (
              <Button size="sm" variant="ghost" className="hover:text-danger" icon={<Ban className="size-3.5" />} onClick={() => onBlock(d, true)}>
                Block
              </Button>
            )}
            <Button size="sm" variant="ghost" className="hover:text-danger" icon={<Trash2 className="size-3.5" />} onClick={() => onRemove(d)}>
              Remove
            </Button>
          </div>
        </div>
      )}
      {editingAccess && <AccessModal device={d} onClose={() => setEditingAccess(false)} />}
    </div>
  )
}

/** Runs one of the fixed commands on a paired device. */
function CommandsButton({ d }: { d: Device }) {
  const [open, setOpen] = useState(false)
  const [list, setList] = useState<{ name: string; label: string; description: string }[] | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState('')
  useEffect(() => {
    if (!open || list) return
    setError('')
    api.deviceCommands(d.id).then(setList).catch((e) => setError(e instanceof Error ? e.message : String(e)))
  }, [open])
  const run = async (name: string) => {
    setBusy(name)
    try {
      const r = await api.runCommand(d.id, name)
      toast(`${d.name}: ${r.message}`)
      setOpen(false)
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(null)
    }
  }
  return (
    <>
      <Button size="sm" variant="ghost" icon={<Zap className="size-3.5" />} onClick={() => setOpen(true)}>Commands</Button>
      <Modal open={open} onClose={() => setOpen(false)} title={`Run on ${d.name}`}>
        {error ? (
          <ErrorNote>{error}</ErrorNote>
        ) : !list ? (
          <Spinner />
        ) : (
          <ul className="divide-y divide-line">
            {list.map((c) => (
              <li key={c.name} className="flex items-center gap-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <div className="font-medium">{c.label}</div>
                  <div className="text-[12px] text-subtle">{c.description}</div>
                </div>
                <Button size="sm" loading={busy === c.name} disabled={busy !== null} onClick={() => void run(c.name)}>Run</Button>
              </li>
            ))}
          </ul>
        )}
      </Modal>
    </>
  )
}

/** Wakes a sleeping computer over the network. */
function WakeButton({ d }: { d: Device }) {
  const [busy, setBusy] = useState(false)
  const wake = async () => {
    setBusy(true)
    try {
      const r = await api.wakeDevice(d.id)
      toast(
        r.relayed_by.length
          ? `Asked ${d.name} to wake up, from here and from ${r.relayed_by.join(', ')}`
          : `Asked ${d.name} to wake up. It wakes only if it is on the same network and Wake-on-LAN is on in its settings.`,
      )
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  return <Button size="sm" icon={<Power className="size-3.5" />} loading={busy} onClick={() => void wake()}>Wake</Button>
}

/** HearthDrive devices on the same local network, and pairing with them. */
function NearbyPanel({ onPair }: { onPair: (link: string) => void }) {
  const [list, setList] = useState<NearbyDevice[] | null>(null)
  useEffect(() => {
    let live = true
    const load = () => api.nearbyDevices().then((l) => live && setList(l)).catch(() => live && setList([]))
    load()
    const poll = setInterval(load, 4000)
    return () => {
      live = false
      clearInterval(poll)
    }
  }, [])
  if (!list || list.length === 0) return null
  return (
    <div className="mt-4">
      <Panel title="On this network">
        <ul className="divide-y divide-line">
          {list.map((n) => {
            const Icon = deviceIcon(n.device_type)
            return (
              <li key={n.id} className="flex items-center gap-3 py-2.5">
                <Icon className="size-4 shrink-0 text-muted" />
                <div className="min-w-0 flex-1">
                  <div className="truncate font-medium">{n.name}</div>
                  <div className="truncate text-[12px] text-subtle">
                    <span className="font-mono">{n.address}</span>
                    {n.fingerprint && <> · <FingerprintText value={n.fingerprint} className="text-[11px]" /></>}
                  </div>
                </div>
                {n.paired ? (
                  <Badge>Paired</Badge>
                ) : n.invite ? (
                  <Button size="sm" variant="primary" icon={<Link2 className="size-3.5" />} onClick={() => onPair(n.invite!)}>Pair</Button>
                ) : (
                  <span className="max-w-[220px] text-right text-[12px] text-subtle">To pair, open Devices → Add device on it</span>
                )}
              </li>
            )
          })}
        </ul>
        <p className="mt-2 flex items-center gap-1.5 text-[12px] text-subtle">
          <Wifi className="size-3.5" /> Compare the fingerprint with the one the other device shows before you approve.
        </p>
      </Panel>
    </div>
  )
}

export function Devices() {
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.') || e.type === 'network.changed', 'devices')
  const requests = useResource(() => api.pairingRequests().catch(() => []), [], (e) => e.type.startsWith('pairing.') || e.type.startsWith('device.'))
  const [inviting, setInviting] = useState(false)
  // `#/devices?join=1` opens the join dialog straight away.
  const [joining, setJoining] = useState(() => new URLSearchParams(window.location.hash.split('?')[1] ?? '').get('join') === '1')
  const [removing, setRemoving] = useState<Device | null>(null)
  const [blocking, setBlocking] = useState<Device | null>(null)
  const [busy, setBusy] = useState(false)
  const [nearbyLink, setNearbyLink] = useState<string | undefined>(undefined)

  // Traffic rates are measured between two looks: look again while a
  // paired device is online.
  const anyOnline = (devices.data ?? []).some((d) => !d.is_local && d.online)
  useEffect(() => {
    if (!anyOnline) return
    const poll = setInterval(() => devices.reload(), 3000)
    return () => clearInterval(poll)
  }, [anyOnline])

  const setBlocked = async (d: Device, blocked: boolean) => {
    setBusy(true)
    try {
      await api.updateDevice(d.id, { blocked })
      toast(blocked ? `Blocked ${d.name}` : `${d.name} can connect again`)
      setBlocking(null)
      devices.reload()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }

  const remove = async () => {
    if (!removing) return
    setBusy(true)
    try {
      await api.removeDevice(removing.id)
      toast(`Removed ${removing.name}`)
      setRemoving(null)
      devices.reload()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }

  const list = devices.data ?? []
  const peers = list.filter((d) => !d.is_local)

  return (
    <div>
      <PageHeader
        title="Devices"
        subtitle="Devices that are part of your cloud"
        actions={
          <>
            <Button icon={<Link2 className="size-3.5" />} onClick={() => setJoining(true)}>{isMobile() ? 'Pair with a computer' : 'Join a cloud'}</Button>
            <Button variant="primary" icon={<QrCode className="size-3.5" />} onClick={() => setInviting(true)}>Add device</Button>
          </>
        }
      />

      {!!requests.data?.length && !inviting && (
        <div className="mb-4 grid grid-cols-[repeat(auto-fill,minmax(320px,1fr))] gap-3">
          {requests.data.map((r) => <RequestCard key={r.id} request={r} onDecided={() => { requests.reload(); devices.reload() }} />)}
        </div>
      )}

      {devices.loading && !devices.data ? (
        <Spinner />
      ) : (
        <div className="grid grid-cols-[repeat(auto-fill,minmax(min(100%,360px),1fr))] gap-4">
          {list.map((d) => <DeviceCard key={`${d.id}-${d.role}-${d.allowed_roots?.join(',') ?? 'all'}`} d={d} introducer={list.find((x) => x.id === d.introduced_by)?.name ?? null} onRemove={setRemoving} onBlock={(d, blocked) => (blocked ? setBlocking(d) : void setBlocked(d, false))} />)}
          {peers.length === 0 && (
            <button
              onClick={() => setInviting(true)}
              className="flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-line p-6 text-center text-muted hover:border-line-strong hover:text-fg"
            >
              <Network className="size-6" />
              <span className="font-medium">Add your next computer</span>
              <span className="text-[12px]">Pair it with a link or QR code. Each computer keeps its files; you browse them from anywhere.</span>
            </button>
          )}
        </div>
      )}

      <NearbyPanel onPair={(link) => { setNearbyLink(link); setJoining(true) }} />

      <IntroductionsToggle />

      <div className="mt-4"><NetworkPanel /></div>

      <div className="mt-4 flex items-start gap-2 text-[12px] text-subtle">
        <ShieldCheck className="mt-0.5 size-3.5 shrink-0" />
        <span>
          Paired devices authenticate every request with their own key. Removing a device takes effect immediately.
          Traffic between devices is encrypted end to end (QUIC with TLS 1.3, tied to each device key). No extra software, no port forwarding, no public exposure.
        </span>
      </div>

      <InviteModal open={inviting} onClose={() => { setInviting(false); requests.reload(); devices.reload() }} />
      <JoinModal open={joining} link={nearbyLink} onClose={() => { setJoining(false); setNearbyLink(undefined); devices.reload() }} />
      <Modal
        open={!!blocking}
        onClose={() => setBlocking(null)}
        title={`Block ${blocking?.name}?`}
        footer={
          <>
            <Button variant="ghost" onClick={() => setBlocking(null)}>Cancel</Button>
            <Button variant="danger" loading={busy} onClick={() => blocking && void setBlocked(blocking, true)}>Block device</Button>
          </>
        }
      >
        <p>
          For a lost or stolen device: <span className="font-medium">{blocking?.name}</span> can no longer reach anything on this {thisDevice()},
          and nothing is synced, copied or backed up to it until you unblock it. It stays paired, so unblocking needs no new pairing.
          Files already on it stay there.
        </p>
      </Modal>
      <Modal
        open={!!removing}
        onClose={() => setRemoving(null)}
        title={`Remove ${removing?.name}?`}
        footer={
          <>
            <Button variant="ghost" onClick={() => setRemoving(null)}>Cancel</Button>
            <Button variant="danger" loading={busy} onClick={() => void remove()}>Remove device</Button>
          </>
        }
      >
        <p>
          <span className="font-medium">{removing?.name}</span> will immediately lose access to this {thisDevice()}, and this {thisDevice()} will stop
          showing its files. No files are deleted on either device. You can pair it again later.
        </p>
      </Modal>
    </div>
  )
}
