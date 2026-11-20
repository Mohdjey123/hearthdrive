import { useCallback, useEffect, useRef, useState } from 'react'
import { FileText } from 'lucide-react'
import { api, peerPrefix, type Device } from '../services/api'
import { followJob } from '../hooks/useJobs'
import { navigate } from '../hooks/useRoute'
import { isScan, scanTarget, setScanTarget } from '../lib/scans'
import { PlacePicker } from '../pages/Sync'
import { toast, toastError } from './Toasts'
import { Button, ErrorNote, Field, Modal, Spinner } from './ui'

/**
 * Sends scans from the phone's document scanner to the computer and folder
 * chosen for them. The first scan asks where that is; later scans go there
 * by themselves (change it on the Documents page). A scan made while that
 * computer is offline waits and goes when the app is next opened.
 */
export function ScanInbox() {
  const [ask, setAsk] = useState<string[] | null>(null)
  const [devices, setDevices] = useState<Device[] | null>(null)
  const [device, setDevice] = useState('')
  const [place, setPlace] = useState({ root: '', path: '' })
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const sending = useRef(false)

  const check = useCallback(async () => {
    if (sending.current) return
    try {
      const scans = (await api.sharedFiles()).filter((f) => isScan(f.name)).map((f) => f.name)
      if (scans.length === 0) return
      const target = scanTarget()
      const all = (await api.devices()).filter((d) => !d.is_local)
      if (!target || !all.some((d) => d.id === target.device_id)) {
        setDevices(all)
        setDevice((current) => current || all.find((d) => d.online)?.id || all[0]?.id || '')
        setAsk(scans)
        return
      }
      if (!all.find((d) => d.id === target.device_id)?.online) {
        toast(`${scans.length === 1 ? 'The scan waits' : `${scans.length} scans wait`} until ${target.device_name} is online.`)
        return
      }
      sending.current = true
      const job = await api.sendShared({ names: scans, device_id: target.device_id, root_id: target.root_id, path: target.path })
      followJob(job.id, `Sending ${scans.length === 1 ? 'the scan' : `${scans.length} scans`} to ${target.device_name}`)
    } catch (e) {
      toastError(e)
    } finally {
      sending.current = false
    }
  }, [])

  useEffect(() => {
    void check()
    const onScanned = () => void check()
    const onFailed = (e: Event) => toast((e as CustomEvent<string>).detail || 'The scanner could not start.')
    const onVisible = () => document.visibilityState === 'visible' && void check()
    window.addEventListener('nexus-scanned', onScanned)
    window.addEventListener('nexus-scan-failed', onFailed)
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      window.removeEventListener('nexus-scanned', onScanned)
      window.removeEventListener('nexus-scan-failed', onFailed)
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [check])

  const online = devices?.find((d) => d.id === device)?.online ?? false

  const save = async () => {
    if (!ask) return
    setBusy(true)
    setError('')
    try {
      const name = devices?.find((d) => d.id === device)?.name ?? 'the computer'
      const job = await api.sendShared({ names: ask, device_id: device, root_id: place.root, path: place.path })
      setScanTarget({ device_id: device, device_name: name, root_id: place.root, path: place.path })
      followJob(job.id, `Sending the scan to ${name}`)
      toast(`Later scans go to ${name}${place.path ? ` › ${place.path}` : ''} too.`)
      setAsk(null)
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  if (!ask) return null
  return (
    <Modal
      open
      onClose={() => setAsk(null)}
      title="Where should scans go?"
      footer={
        <>
          <Button variant="ghost" onClick={() => setAsk(null)}>Later</Button>
          <Button variant="primary" loading={busy} disabled={!online || !place.root} onClick={() => void save()}>Save here</Button>
        </>
      }
    >
      <div className="space-y-4">
        <div className="flex items-center gap-2.5 rounded-md border border-line px-3 py-2">
          <FileText className="size-4 shrink-0 text-subtle" />
          <span className="min-w-0 flex-1 truncate">{ask.length === 1 ? ask[0] : `${ask.length} scans`}</span>
        </div>
        <p className="text-muted">This scan and the ones after it go to this folder. HearthDrive reads them there, so you can search them on the Documents page.</p>
        {devices === null ? (
          <Spinner />
        ) : devices.length === 0 ? (
          <div className="space-y-2">
            <p className="text-muted">Pair this phone with a computer to keep scans on it.</p>
            <Button size="sm" onClick={() => { setAsk(null); navigate('devices') }}>Go to Devices</Button>
          </div>
        ) : (
          <>
            <Field label="Computer">
              <select
                value={device}
                onChange={(e) => {
                  setDevice(e.target.value)
                  setPlace({ root: '', path: '' })
                }}
                className="h-8 w-full rounded-md border border-line bg-bg px-2 text-fg outline-none focus:border-accent"
              >
                {devices.map((d) => (
                  <option key={d.id} value={d.id}>{d.name}{d.online ? '' : ' (offline)'}</option>
                ))}
              </select>
            </Field>
            <Field label="Folder">
              {online ? (
                <PlacePicker prefix={peerPrefix(device)} writable value={place} onChange={setPlace} />
              ) : (
                <p className="text-muted">This computer is offline. Press Later; the scan waits on the phone.</p>
              )}
            </Field>
          </>
        )}
        <ErrorNote>{error}</ErrorNote>
      </div>
    </Modal>
  )
}
