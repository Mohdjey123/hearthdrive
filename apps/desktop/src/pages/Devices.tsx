import { useState } from 'react'
import {
  FolderOpen, Laptop, Link2, Monitor, Network, QrCode, RefreshCw, ShieldCheck, Smartphone, Trash2,
} from 'lucide-react'
import { api, isMobile, thisDevice, type Device, type NetworkInfo, type Role } from '../services/api'
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

function DeviceCard({ d, onRemove }: { d: Device; onRemove: (d: Device) => void }) {
  const Icon = deviceIcon(d.device_type)
  const [role, setRole] = useState<Role>(d.role)
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
          </div>
          <div className="mt-0.5 flex items-center gap-1.5 text-[12px] text-muted">
            <StatusDot tone={d.online ? 'online' : 'offline'} />
            {d.is_local ? 'Online' : d.online ? `Online${d.latency_ms != null ? ` · ${d.latency_ms} ms` : ''}` : d.last_seen ? `Offline · seen ${formatRelative(d.last_seen)}` : 'Offline · not seen yet'}
          </div>
        </div>
      </div>
      <dl className="mt-4 grid grid-cols-2 gap-3 text-[12px]">
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
              <dd className="mt-0.5 truncate text-[12px]">
                {(() => {
                  const r = routeParts((d.route ?? d.network_address)!)
                  return <><span className="font-medium">{r.kind}</span> <span className="font-mono text-[11.5px] text-muted">· {r.where}</span></>
                })()}
              </dd>
            </div>
          )
        )}
      </dl>
      {!d.is_local && (
        <div className="mt-4 flex gap-2 border-t border-line pt-3">
          <Button size="sm" icon={<FolderOpen className="size-3.5" />} onClick={() => navigate('files', { device: d.id })}>Browse files</Button>
          <Button size="sm" variant="ghost" className="ml-auto hover:text-danger" icon={<Trash2 className="size-3.5" />} onClick={() => onRemove(d)}>
            Remove
          </Button>
        </div>
      )}
    </div>
  )
}

export function Devices() {
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.') || e.type === 'network.changed')
  const requests = useResource(() => api.pairingRequests().catch(() => []), [], (e) => e.type.startsWith('pairing.') || e.type.startsWith('device.'))
  const [inviting, setInviting] = useState(false)
  // `#/devices?join=1` opens the join dialog straight away.
  const [joining, setJoining] = useState(() => new URLSearchParams(window.location.hash.split('?')[1] ?? '').get('join') === '1')
  const [removing, setRemoving] = useState<Device | null>(null)
  const [busy, setBusy] = useState(false)

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
        <div className="grid grid-cols-[repeat(auto-fill,minmax(260px,1fr))] gap-4">
          {list.map((d) => <DeviceCard key={`${d.id}-${d.role}`} d={d} onRemove={setRemoving} />)}
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

      <div className="mt-4"><NetworkPanel /></div>

      <div className="mt-4 flex items-start gap-2 text-[12px] text-subtle">
        <ShieldCheck className="mt-0.5 size-3.5 shrink-0" />
        <span>
          Paired devices authenticate every request with their own key. Removing a device takes effect immediately.
          Traffic between devices is encrypted end to end (QUIC with TLS 1.3, tied to each device key). No extra software, no port forwarding, no public exposure.
        </span>
      </div>

      <InviteModal open={inviting} onClose={() => { setInviting(false); requests.reload(); devices.reload() }} />
      <JoinModal open={joining} onClose={() => { setJoining(false); devices.reload() }} />
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
