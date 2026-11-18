import { useState } from 'react'
import { Download, RefreshCw } from 'lucide-react'
import { androidBridge, api, isMobile, type UpdateStatus } from '../services/api'
import { useResource } from '../hooks/useResource'
import { formatBytes, formatRelative } from '../lib/format'
import { toast, toastError } from './Toasts'
import { Badge, Button, Spinner } from './ui'

const PLATFORM_NAMES: Record<string, string> = {
  'windows-x86_64': 'Windows',
  'android-aarch64': 'Android',
}

export const platformName = (p: string) => PLATFORM_NAMES[p] ?? p

/** Update status, refreshed whenever the node reports a change. */
export function useUpdates() {
  return useResource(() => api.updates(), [], (e) => e.type === 'updates.changed')
}

/**
 * Installs the waiting update. On Windows the node starts the installer,
 * which closes this window and opens the new version; on Android, Android's
 * own installer asks first.
 */
export async function installUpdate(status: UpdateStatus): Promise<void> {
  if (status.install === 'app') {
    const bridge = androidBridge()
    if (!bridge?.installUpdate || !status.package_path) throw new Error('This app cannot install updates itself. Install the new APK by hand.')
    const result = bridge.installUpdate(status.package_path)
    if (result === 'permission') toast('Allow NEXUS to install apps, then tap Install again.')
    else if (result === 'missing') throw new Error('The update is no longer there. Check for updates again.')
    return
  }
  await api.installUpdate()
  toast(`Installing NEXUS ${status.available?.version}. This window closes and opens again when it is done.`)
}

/** A strip at the top of every page while an update is ready. */
export function UpdateBanner() {
  const updates = useUpdates()
  const [busy, setBusy] = useState(false)
  const [dismissed, setDismissed] = useState<string | null>(null)
  const s = updates.data
  const ready = s?.available
  if (!s || !ready || s.install === 'none' || dismissed === ready.version) return null

  const install = async () => {
    setBusy(true)
    try {
      await installUpdate(s)
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="mb-4 flex flex-wrap items-center gap-3 rounded-lg border border-line bg-accent-soft px-4 py-3">
      <Download className="size-4 shrink-0 text-accent" />
      <div className="min-w-0 flex-1">
        <div className="font-medium">NEXUS {ready.version} is ready</div>
        <div className="text-[12.5px] text-muted">
          {s.install === 'app'
            ? 'Android asks you to confirm. Your files and settings stay.'
            : s.auto_install
              ? 'It installs by itself once this window is closed, or now if you like.'
              : 'Installing takes a few seconds and opens NEXUS again.'}
        </div>
      </div>
      <Button size="sm" variant="ghost" onClick={() => setDismissed(ready.version)}>Later</Button>
      <Button size="sm" variant="primary" loading={busy} onClick={() => void install()}>Install now</Button>
    </div>
  )
}

/** The Updates section on the Settings page. */
export function UpdateSettings() {
  const updates = useUpdates()
  const [busy, setBusy] = useState(false)
  const s = updates.data
  if (!s) return updates.error ? <p className="text-subtle">{updates.error.message}</p> : <Spinner />

  const check = async () => {
    try {
      await api.checkUpdates()
      updates.reload()
    } catch (e) {
      toastError(e)
    }
  }
  const setAuto = async (on: boolean) => {
    setBusy(true)
    try {
      await api.setAutoUpdate(on)
      updates.reload()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  const install = async () => {
    setBusy(true)
    try {
      await installUpdate(s)
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }

  const state = s.downloading
    ? `Receiving ${s.downloading.replace(/^\S+/, (p) => platformName(p))}…`
    : s.checking
      ? 'Asking your other devices…'
      : s.error
        ? `Last check failed: ${s.error}`
        : s.last_checked
          ? `Checked ${formatRelative(s.last_checked)}`
          : 'Not checked yet'

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        {s.available ? (
          <>
            <Badge tone="accent">NEXUS {s.available.version} is ready</Badge>
            {s.install !== 'none' && (
              <Button size="sm" variant="primary" icon={<Download className="size-3.5" />} loading={busy} onClick={() => void install()}>
                Install now
              </Button>
            )}
          </>
        ) : (
          <span>NEXUS {s.version} is up to date.</span>
        )}
        <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} disabled={s.checking} onClick={() => void check()}>
          Check now
        </Button>
      </div>
      <div className="text-[12px] text-subtle">{state}</div>
      {s.install_note && <div className="text-[12.5px] text-muted">{s.install_note}</div>}
      {s.available?.notes && (
        <div className="whitespace-pre-wrap rounded-md border border-line bg-elevated px-3 py-2 text-[12.5px]">{s.available.notes}</div>
      )}

      {s.install === 'service' && (
        <label className="flex items-start gap-2.5">
          <input
            type="checkbox"
            className="mt-0.5 accent-[var(--nx-accent)]"
            checked={s.auto_install}
            disabled={busy || !s.can_configure}
            onChange={(e) => void setAuto(e.target.checked)}
          />
          <span>
            Install updates automatically
            <span className="mt-0.5 block text-[12px] text-subtle">
              When this window is closed and no copy or sync is running. The service restarts in a few seconds.
            </span>
          </span>
        </label>
      )}

      <p className="text-[12px] text-subtle">
        Updates come from your own devices: each passes signed packages on to the others, and NEXUS checks the signature before keeping or
        installing one.
        {!isMobile() && s.carried.length > 0 && (
          <> Passing on: {s.carried.map((r) => `${platformName(r.platform)} ${r.version} (${formatBytes(r.size)})`).join(', ')}.</>
        )}
      </p>
    </div>
  )
}
