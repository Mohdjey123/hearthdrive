import { useEffect, useState } from 'react'
import { Smartphone } from 'lucide-react'
import { androidBridge, api, isMobile, type PhoneStorage } from '../services/api'
import { useResource } from '../hooks/useResource'
import { toast, toastError } from './Toasts'
import { Button, EmptyState } from './ui'

/**
 * The phone's own storage in HearthDrive (phones only): whether it is shown,
 * and whether paired computers may browse it. `null` on computers.
 */
export function usePhoneStorage() {
  return useResource<PhoneStorage | null>(
    () => (isMobile() ? api.phoneStorage() : Promise.resolve(null)),
    [],
    (e) => e.type === 'storage.changed',
    'phone-storage',
  )
}

/** Whether Android lets HearthDrive read all files (true outside the Android app). */
function hasAllFiles(): boolean {
  const b = androidBridge()
  return !b?.hasAllFiles || b.hasAllFiles()
}

/** Asks Android for access to all files, resolving once the person is back. */
function askAllFiles(): Promise<boolean> {
  const b = androidBridge()
  if (!b?.requestAllFiles) return Promise.resolve(hasAllFiles())
  return new Promise((resolve) => {
    const done = () => {
      window.removeEventListener('nexus-all-files', done)
      resolve(hasAllFiles())
    }
    window.addEventListener('nexus-all-files', done)
    b.requestAllFiles?.()
  })
}

/** Shows the phone's storage: asks for All files access first, then adds it. */
export async function showPhoneStorage(): Promise<boolean> {
  try {
    if (!hasAllFiles() && !(await askAllFiles())) {
      toast("HearthDrive needs All files access to show this phone's files", 'error')
      return false
    }
    await api.setPhoneStorage({ enabled: true })
    toast("This phone's files are in HearthDrive now")
    return true
  } catch (e) {
    toastError(e)
    return false
  }
}

/** Files and Photos on a phone that does not show its own storage yet. */
export function PhoneStorageEmpty({ onShown }: { onShown?: () => void }) {
  const [busy, setBusy] = useState(false)
  return (
    <EmptyState
      icon={<Smartphone className="size-8" />}
      title="Show this phone's files"
      action={
        <Button
          variant="primary"
          loading={busy}
          onClick={async () => {
            setBusy(true)
            if (await showPhoneStorage()) onShown?.()
            setBusy(false)
          }}
        >
          Show this phone's files
        </Button>
      }
    >
      Browse the phone's storage here: Downloads, DCIM, Documents and the rest. Android asks you to allow "All files access" first. Your
      paired computers can browse it too, unless you turn that off in Settings.
    </EmptyState>
  )
}

/** Settings on a phone: show its storage, and let computers browse it or not. */
export function PhoneStorageSettings() {
  const phone = usePhoneStorage()
  const [busy, setBusy] = useState(false)
  const [allowed, setAllowed] = useState(hasAllFiles)
  // Back from Android's settings, the access may have changed.
  useEffect(() => {
    const check = () => setAllowed(hasAllFiles())
    window.addEventListener('nexus-all-files', check)
    document.addEventListener('visibilitychange', check)
    return () => {
      window.removeEventListener('nexus-all-files', check)
      document.removeEventListener('visibilitychange', check)
    }
  }, [])
  const p = phone.data
  if (!p?.available) return null
  const shown = !!p.root_id
  const change = async (b: { enabled?: boolean; private?: boolean }) => {
    setBusy(true)
    try {
      await api.setPhoneStorage(b)
      await phone.reload()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }
  return (
    <div className="space-y-3">
      <p className="text-muted">
        {shown
          ? "This phone's storage shows in Files and Photos."
          : "Show this phone's storage (Downloads, DCIM, Documents and the rest) in Files and Photos."}
      </p>
      {shown && !allowed && (
        <p className="text-[12.5px] text-danger">
          Android no longer lets HearthDrive read all files, so the phone's storage looks empty.{' '}
          <button className="underline" onClick={() => void askAllFiles().then(setAllowed)}>Allow again</button>
        </p>
      )}
      {shown && (
        <label className="flex items-start gap-2">
          <input
            type="checkbox"
            className="mt-1 accent-accent"
            checked={!p.private}
            disabled={busy}
            onChange={(e) => void change({ private: !e.target.checked })}
          />
          <span>
            Let my computers browse this phone
            <span className="block text-[12px] text-subtle">
              Paired computers see the phone's storage in Files while HearthDrive runs on the phone. Off: it stays on the phone.
            </span>
          </span>
        </label>
      )}
      <div>
        {shown ? (
          <Button size="sm" loading={busy} onClick={() => void change({ enabled: false })}>Stop showing the phone's files</Button>
        ) : (
          <Button
            size="sm"
            variant="primary"
            loading={busy}
            onClick={async () => {
              setBusy(true)
              await showPhoneStorage()
              await phone.reload()
              setBusy(false)
            }}
          >
            Show this phone's files
          </Button>
        )}
      </div>
    </div>
  )
}
