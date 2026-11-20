import { useState } from 'react'
import { AlertTriangle, CheckCircle2, ClipboardCopy, HardDrive, RefreshCw, ShieldCheck, Usb, Wrench, XCircle } from 'lucide-react'
import { api, peerPrefix, type DamagedFile, type Device, type PhysicalDisk, type RootHealth, type StorageHealth } from '../services/api'
import { useResource } from '../hooks/useResource'
import { JobProgress } from '../components/JobProgress'
import { reloadJobs, useJobFor } from '../hooks/useJobs'
import { formatBytes, formatDate, formatRelative } from '../lib/format'
import { toast, toastError } from '../components/Toasts'
import { Badge, Button, ErrorNote, Modal, PageHeader, Panel, Progress, ProgressBar, Spinner, StatusDot, cx } from '../components/ui'

const HEALTH: Record<PhysicalDisk['health'], { tone: 'accent' | 'warn' | 'danger' | 'neutral'; label: string }> = {
  healthy: { tone: 'accent', label: 'Healthy' },
  warning: { tone: 'warn', label: 'Warning' },
  unhealthy: { tone: 'danger', label: 'Failing' },
  unknown: { tone: 'neutral', label: 'Unknown' },
}

function Problems({ health }: { health: StorageHealth }) {
  if (health.problems.length === 0) {
    return (
      <div className="flex items-center gap-2 text-[13px]">
        <CheckCircle2 className="size-4 text-accent" /> Nothing needs attention.
      </div>
    )
  }
  return (
    <ul className="space-y-1.5">
      {health.problems.map((p, i) => (
        <li key={i} className="flex items-start gap-2 text-[13px]">
          {p.level === 'error' ? <XCircle className="mt-0.5 size-4 shrink-0 text-danger" /> : <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warn" />}
          <span>{p.text}</span>
        </li>
      ))}
    </ul>
  )
}

function DiskCard({ disk }: { disk: PhysicalDisk }) {
  const h = HEALTH[disk.health]
  const facts: string[] = []
  if (disk.temperature_c != null) facts.push(`${disk.temperature_c} °C`)
  if (disk.wear_percent != null) facts.push(`${disk.wear_percent}% of rated life used`)
  if (disk.power_on_hours != null) facts.push(`${Math.round(disk.power_on_hours / 24)} days powered on`)
  if (disk.read_errors != null || disk.write_errors != null) {
    const n = (disk.read_errors ?? 0) + (disk.write_errors ?? 0)
    facts.push(n ? `${n} uncorrected errors` : 'no uncorrected errors')
  }
  return (
    <li className="rounded-md border border-line p-3">
      <div className="flex items-start gap-2">
        {disk.bus === 'USB' ? <Usb className="mt-0.5 size-4 text-muted" /> : <HardDrive className="mt-0.5 size-4 text-muted" />}
        <div className="min-w-0 flex-1">
          <div className="truncate font-medium" title={disk.model}>{disk.model}</div>
          <div className="text-[12px] text-subtle">
            {[disk.media === 'unknown' ? null : disk.media.toUpperCase(), disk.bus, formatBytes(disk.size), disk.letters.length ? disk.letters.map((l) => `${l}:`).join(' ') : null]
              .filter(Boolean)
              .join(' · ')}
          </div>
        </div>
        <Badge tone={h.tone}>{h.label}</Badge>
      </div>
      {disk.status && disk.status !== 'OK' && <p className="mt-2 text-[12px] text-warn">Windows says: {disk.status}</p>}
      {facts.length > 0 && <p className="mt-2 text-[12px] text-muted">{facts.join(' · ')}</p>}
      {disk.storage.length > 0 && <p className="mt-1 text-[12px] text-subtle">Holds {disk.storage.join(', ')}</p>}
    </li>
  )
}

function DamagedList({ root, onChanged, readOnly }: { root: RootHealth; onChanged: () => void; readOnly: boolean }) {
  const [busy, setBusy] = useState<string | null>(null)
  const [accepting, setAccepting] = useState<DamagedFile | null>(null)
  const act = async (id: string, fn: () => Promise<unknown>, done: string) => {
    setBusy(id)
    try {
      await fn()
      toast(done)
      onChanged()
      reloadJobs()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(null)
    }
  }
  return (
    <div className="mt-3 rounded-md border border-danger/40 bg-danger-soft/40">
      <p className="px-3 pt-2.5 text-[12.5px]">
        {root.damaged.length === 1 ? 'This file has' : 'These files have'} changed on the disk although nobody edited them — a sign the drive is
        damaging data. Repairing puts back the right content from a copy or a backup; what is there now is kept as a version.
      </p>
      <ul className="divide-y divide-line">
        {root.damaged.map((d) => (
          <li key={d.file_id} className="flex flex-wrap items-center gap-x-3 gap-y-1.5 px-3 py-2">
            <RepairProgress fileId={d.file_id} />
            <div className="min-w-0 flex-1">
              <div className="truncate font-medium" title={d.path}>{d.path}</div>
              <div className="text-[12px] text-subtle">
                {formatBytes(d.size)} · {d.unreadable ? 'cannot be read' : 'content changed'} · found {formatRelative(d.found_at)}
                {d.repair_from.length > 0 ? ` · a good copy is in ${d.repair_from.join(', ')}` : ' · no copy or backup holds it'}
              </div>
            </div>
            {!readOnly && (
              <span className="flex gap-1">
                <Button
                  size="sm"
                  variant="primary"
                  disabled={d.repair_from.length === 0 || busy === d.file_id}
                  onClick={() => void act(d.file_id, () => api.repairDamaged(d.file_id), 'Repairing')}
                >
                  <Wrench className="size-3.5" /> Repair
                </Button>
                {!d.unreadable && (
                  <Button size="sm" variant="ghost" disabled={busy === d.file_id} onClick={() => setAccepting(d)}>It is fine</Button>
                )}
              </span>
            )}
          </li>
        ))}
      </ul>
      <Modal
        open={!!accepting}
        onClose={() => setAccepting(null)}
        title="Keep this file as it is now?"
        footer={
          <>
            <Button variant="ghost" onClick={() => setAccepting(null)}>Cancel</Button>
            <Button
              variant="primary"
              loading={busy === accepting?.file_id}
              onClick={() => accepting && void act(accepting.file_id, () => api.acceptDamaged(accepting.file_id), 'Noted').then(() => setAccepting(null))}
            >
              Keep it as it is
            </Button>
          </>
        }
      >
        <p>
          Choose this only if {accepting?.path} was changed on purpose by a program that keeps the file's date (some do). HearthDrive will treat its
          current content as the right one.
        </p>
      </Modal>
    </div>
  )
}

/** A running repair of one damaged file, on its own line. */
function RepairProgress({ fileId }: { fileId: string }) {
  const job = useJobFor(fileId)
  return job ? <JobProgress job={job} label="Repairing" className="order-last w-full" /> : null
}

/** "1,204 files (3.1 GB) read, all intact" — counted afresh, as repairs lower the damaged count. */
function checkText(c: NonNullable<RootHealth['last_check']>) {
  const files = `${c.files} file${c.files === 1 ? '' : 's'} (${formatBytes(c.bytes)}) read`
  return c.damaged ? `${files}, ${c.damaged} damaged` : `${files}, all intact`
}

function RootCard({ root, onChanged, readOnly }: { root: RootHealth; onChanged: () => void; readOnly: boolean }) {
  const [busy, setBusy] = useState(false)
  const job = useJobFor(root.id)
  const used = root.total_bytes && root.available_bytes != null ? 1 - root.available_bytes / root.total_bytes : null
  const verify = async () => {
    setBusy(true)
    try {
      await api.verifyRoot(root.id)
      toast(`Verifying ${root.name}. It reads every file, so it can take a while.`)
      onChanged()
      reloadJobs()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  return (
    <li className="px-4 py-3">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5 font-medium">
            <StatusDot tone={root.online ? (root.verifying ? 'busy' : 'online') : 'error'} />
            <span className="truncate" title={root.path}>{root.name}</span>
            {root.removable && <Badge>Removable</Badge>}
          </div>
          <div className="truncate text-[12px] text-subtle" title={root.path}>{root.path}</div>
        </div>
        {!readOnly && (
          <Button size="sm" disabled={busy || root.verifying || !root.online} onClick={() => void verify()}>
            <ShieldCheck className={cx('size-3.5', root.verifying && 'animate-pulse')} /> {root.verifying ? 'Verifying…' : 'Verify files'}
          </Button>
        )}
      </div>
      <div className="mt-2 grid grid-cols-1 gap-x-6 gap-y-1.5 text-[12.5px] md:grid-cols-2">
        <div>
          {root.online ? (
            used != null ? (
              <>
                <ProgressBar value={used} tone={used > 0.97 ? 'danger' : used > 0.9 ? 'warn' : 'accent'} />
                <div className="mt-1 text-muted">{formatBytes(root.available_bytes)} free of {formatBytes(root.total_bytes)}</div>
              </>
            ) : (
              <span className="text-muted">Free space unknown</span>
            )
          ) : (
            <span className="text-danger">Missing{root.since ? ` since ${formatDate(root.since)}` : ''}: its drive or folder cannot be reached</span>
          )}
        </div>
        <div className="text-muted">
          {root.last_check ? (
            <>Verified {formatRelative(root.last_check.checked_at)}: {checkText(root.last_check)}</>
          ) : (
            'Not verified yet. HearthDrive reads every file once a month to find damage.'
          )}
        </div>
      </div>
      {job && <JobProgress job={job} className="mt-3" />}
      {root.damaged.length > 0 && <DamagedList root={root} onChanged={onChanged} readOnly={readOnly} />}
    </li>
  )
}

function HealthBody({ health, onChanged, readOnly }: { health: StorageHealth; onChanged: () => void; readOnly: boolean }) {
  const noDetails = health.disks.length > 0 && health.disks.every((d) => !d.details)
  return (
    <div className="space-y-4">
      <Panel title="Needs attention">
        <Problems health={health} />
      </Panel>
      <Panel title="Disks" actions={health.checked_at ? <span className="text-[12px] text-subtle">Read {formatRelative(health.checked_at)}</span> : undefined}>
        {health.disks_error && <ErrorNote>{health.disks_error}</ErrorNote>}
        {health.disks.length === 0 ? (
          <p className="text-muted">{health.checked_at ? 'Windows reported no disks.' : 'Reading the disks…'}</p>
        ) : (
          <ul className="grid grid-cols-1 gap-2 md:grid-cols-2">
            {health.disks.map((d) => <DiskCard key={d.number} disk={d} />)}
          </ul>
        )}
        {noDetails && (
          <p className="mt-2 text-[12px] text-subtle">
            Windows only shares temperature, wear and error counts with programs running as administrator, so they are not shown here. Its own
            health verdict above is still checked every half hour.
          </p>
        )}
      </Panel>
      <Panel title="Storage locations" padded={false}>
        {health.roots.length === 0 ? (
          <p className="p-4 text-muted">No storage added yet.</p>
        ) : (
          <ul className="divide-y divide-line">
            {health.roots.map((r) => <RootCard key={r.id} root={r} onChanged={onChanged} readOnly={readOnly} />)}
          </ul>
        )}
      </Panel>
    </div>
  )
}

function DeviceHealth({ device }: { device: Device }) {
  const [open, setOpen] = useState(false)
  const health = useResource(
    async () => (device.online ? api.storageHealth(peerPrefix(device.id)) : null),
    [device.id, device.online],
    (e) => e.type === 'device.online' || e.type === 'device.offline',
    `health:${device.id}`,
  )
  const h = health.data
  const errors = h?.problems.filter((p) => p.level === 'error').length ?? 0
  const warnings = (h?.problems.length ?? 0) - errors
  return (
    <li className="rounded-lg border border-line bg-panel">
      <button className="flex w-full items-center gap-2 px-4 py-3 text-left hover:bg-hover" onClick={() => setOpen((o) => !o)} disabled={!h}>
        <StatusDot tone={!device.online ? 'offline' : errors ? 'error' : warnings ? 'busy' : 'online'} />
        <span className="flex-1 font-medium">{device.name}</span>
        <span className="text-[12.5px] text-muted">
          {!device.online
            ? 'Offline'
            : health.error
              ? health.error.message.includes('does not support') ? `Update HearthDrive on ${device.name} to see its drive health` : 'Could not ask'
              : !h
                ? 'Asking…'
                : errors || warnings
                  ? [errors && `${errors} to fix`, warnings && `${warnings} to watch`].filter(Boolean).join(', ')
                  : `${h.disks.length} disk${h.disks.length === 1 ? '' : 's'}, all fine`}
        </span>
      </button>
      {open && h && (
        <div className="border-t border-line p-3">
          <HealthBody health={h} onChanged={health.reload} readOnly />
          <p className="mt-2 text-[12px] text-subtle">To verify or repair files there, open HearthDrive on {device.name}.</p>
        </div>
      )}
    </li>
  )
}

/** A plain-text report of every device's drives, for keeping or sending. */
function report(here: StorageHealth, name: string): string {
  const lines = [`HearthDrive drive health report, ${new Date().toLocaleString()}`, '', `== ${name} ==`]
  lines.push(here.problems.length ? 'Needs attention:' : 'Nothing needs attention.')
  for (const p of here.problems) lines.push(`  [${p.level}] ${p.text}`)
  lines.push('', 'Disks:')
  for (const d of here.disks) {
    const extra = [d.temperature_c != null && `${d.temperature_c} °C`, d.wear_percent != null && `${d.wear_percent}% worn`].filter(Boolean).join(', ')
    lines.push(`  ${d.model} (${d.letters.map((l) => `${l}:`).join(' ') || 'no letter'}), ${d.media.toUpperCase()} ${d.bus}, ${formatBytes(d.size)}: ${HEALTH[d.health].label}${extra ? `, ${extra}` : ''}`)
  }
  lines.push('', 'Storage locations:')
  for (const r of here.roots) {
    const space = r.total_bytes ? `${formatBytes(r.available_bytes)} free of ${formatBytes(r.total_bytes)}` : 'free space unknown'
    const check = r.last_check ? `verified ${formatDate(r.last_check.checked_at)}: ${checkText(r.last_check)}` : 'not verified yet'
    lines.push(`  ${r.name} (${r.path}): ${r.online ? 'online' : 'MISSING'}, ${space}, ${check}`)
    for (const d of r.damaged) lines.push(`    damaged: ${d.path}`)
  }
  return lines.join('\n')
}

export function Health() {
  const health = useResource(
    () => api.storageHealth(),
    [],
    (e) => e.type === 'health.updated' || e.type.startsWith('storage.') || e.type === 'sync.updated',
    'health',
  )
  const devices = useResource(() => api.devices(), [], (e) => e.type.startsWith('device.'), 'devices')
  const peers = (devices.data ?? []).filter((d) => !d.is_local && d.public_key && d.device_type !== 'android')
  const here = devices.data?.find((d) => d.is_local)?.name ?? 'This computer'
  const [refreshing, setRefreshing] = useState(false)

  const refresh = async () => {
    setRefreshing(true)
    try {
      await api.refreshHealth()
      health.reload()
    } catch (e) {
      toastError(e)
    } finally {
      setRefreshing(false)
    }
  }
  const copyReport = async () => {
    if (!health.data) return
    try {
      await navigator.clipboard.writeText(report(health.data, here))
      toast('Report copied')
    } catch (e) {
      toastError(e)
    }
  }

  return (
    <div>
      <PageHeader
        title="Drive health"
        subtitle="Disks, free space and damaged files on this computer and your other devices"
        actions={
          <>
            <Button disabled={!health.data} onClick={() => void copyReport()}><ClipboardCopy className="size-4" /> Copy report</Button>
            <Button loading={refreshing} onClick={() => void refresh()}><RefreshCw className="size-4" /> Check now</Button>
          </>
        }
      />
      {refreshing && <Progress className="mb-4 max-w-md" label="Reading the disks and checking every drive…" />}
      {health.error && <div className="mb-3"><ErrorNote>{health.error.message}</ErrorNote></div>}
      {!health.data ? <Spinner /> : <HealthBody health={health.data} onChanged={health.reload} readOnly={false} />}
      {peers.length > 0 && (
        <section className="mt-6">
          <h2 className="mb-2 text-[13px] font-medium text-muted">Other devices</h2>
          <ul className="space-y-2">
            {peers.map((d) => <DeviceHealth key={d.id} device={d} />)}
          </ul>
        </section>
      )}
    </div>
  )
}

/** The short version for the Overview: problems on this computer, if any. */
export function HealthAlert({ onOpen }: { onOpen: () => void }) {
  const health = useResource(() => api.storageHealth(), [], (e) => e.type === 'health.updated' || e.type.startsWith('storage.'), 'health')
  const problems = health.data?.problems ?? []
  if (problems.length === 0) return null
  const errors = problems.filter((p) => p.level === 'error')
  const shown = (errors.length ? errors : problems).slice(0, 3)
  return (
    <div className={cx('mb-4 rounded-lg border p-3', errors.length ? 'border-danger/50 bg-danger-soft/40' : 'border-warn/50 bg-warn-soft/40')}>
      <div className="flex items-start gap-2">
        {errors.length ? <XCircle className="mt-0.5 size-4 shrink-0 text-danger" /> : <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warn" />}
        <ul className="min-w-0 flex-1 space-y-1 text-[13px]">
          {shown.map((p, i) => <li key={i}>{p.text}</li>)}
          {problems.length > shown.length && <li className="text-muted">and {problems.length - shown.length} more</li>}
        </ul>
        <Button size="sm" onClick={onOpen}>Drive health</Button>
      </div>
    </div>
  )
}
