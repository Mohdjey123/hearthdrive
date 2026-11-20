import { useCallback, useEffect, useState } from 'react'
import { FileIcon } from 'lucide-react'
import { api, peerPrefix, type Device, type SharedFile } from '../services/api'
import { navigate } from '../hooks/useRoute'
import { formatBytes } from '../lib/format'
import { PlacePicker } from '../pages/Sync'
import { isScan } from '../lib/scans'
import { toast, toastError } from './Toasts'
import { Button, ErrorNote, Field, Modal, Spinner } from './ui'

const SHOWN = 6

/**
 * Files shared to HearthDrive from other apps (Android's share menu) wait in the
 * app; this asks where to send them.
 */
export function ShareInbox() {
  const [files, setFiles] = useState<SharedFile[]>([])
  const [open, setOpen] = useState(false)
  const [devices, setDevices] = useState<Device[] | null>(null)
  const [device, setDevice] = useState('')
  const [place, setPlace] = useState({ root: '', path: '' })
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  const check = useCallback(async () => {
    try {
      // Scans have their own way out (ScanInbox).
      const list = (await api.sharedFiles()).filter((f) => !isScan(f.name))
      setFiles(list)
      if (list.length > 0) {
        setOpen(true)
        const all = (await api.devices()).filter((d) => !d.is_local)
        setDevices(all)
        setDevice((current) => current || all.find((d) => d.online)?.id || all[0]?.id || '')
      }
    } catch {
      /* the node is starting; the next share or visit checks again */
    }
  }, [])

  useEffect(() => {
    void check()
    const onShared = () => void check()
    const onVisible = () => document.visibilityState === 'visible' && void check()
    window.addEventListener('nexus-shared', onShared)
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      window.removeEventListener('nexus-shared', onShared)
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [check])

  const online = devices?.find((d) => d.id === device)?.online ?? false
  const total = files.reduce((n, f) => n + f.size, 0)
  const title = files.length === 1 ? `Send “${files[0].name}”` : `Send ${files.length} files`

  const send = async () => {
    setBusy(true)
    setError('')
    try {
      await api.sendShared({ names: files.map((f) => f.name), device_id: device, root_id: place.root, path: place.path })
      const name = devices?.find((d) => d.id === device)?.name ?? 'the computer'
      toast(`Sending to ${name}. Follow it in Transfers.`)
      setOpen(false)
      setFiles([])
      navigate('transfers')
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  const discard = async () => {
    try {
      await api.discardShared(files.map((f) => f.name))
      setFiles([])
      setOpen(false)
    } catch (e) {
      toastError(e)
    }
  }

  if (!open || files.length === 0) return null
  return (
    <Modal
      open
      onClose={() => setOpen(false)}
      title={title}
      footer={
        <>
          <Button variant="ghost" className="hover:text-danger" onClick={() => void discard()}>Discard</Button>
          <Button variant="ghost" onClick={() => setOpen(false)}>Later</Button>
          <Button variant="primary" loading={busy} disabled={!online || !place.root} onClick={() => void send()}>Send</Button>
        </>
      }
    >
      <div className="space-y-4">
        <ul className="divide-y divide-line rounded-md border border-line">
          {files.slice(0, SHOWN).map((f) => (
            <li key={f.name} className="flex items-center gap-2.5 px-3 py-2">
              <FileIcon className="size-4 shrink-0 text-subtle" />
              <span className="min-w-0 flex-1 truncate">{f.name}</span>
              <span className="shrink-0 text-[12px] text-subtle tabular">{formatBytes(f.size)}</span>
            </li>
          ))}
          {files.length > SHOWN && (
            <li className="px-3 py-2 text-[12.5px] text-subtle">
              and {files.length - SHOWN} more · {formatBytes(total)} in all
            </li>
          )}
        </ul>
        {devices === null ? (
          <Spinner />
        ) : devices.length === 0 ? (
          <div className="space-y-2">
            <p className="text-muted">Pair this phone with a computer to send files to it.</p>
            <Button size="sm" onClick={() => { setOpen(false); navigate('devices') }}>Go to Devices</Button>
          </div>
        ) : (
          <>
            <Field label="To computer">
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
                <p className="text-muted">This computer is offline. Press Later and send these when it is back.</p>
              )}
            </Field>
          </>
        )}
        <ErrorNote>{error}</ErrorNote>
      </div>
    </Modal>
  )
}
