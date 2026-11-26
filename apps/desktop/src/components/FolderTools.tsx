import { useEffect, useRef, useState } from 'react'
import { Lock, LockOpen, Puzzle } from 'lucide-react'
import { toolsApi, type EncryptionState, type FileEntry, type PluginCommand, type PluginRun } from '../services/api'
import { followJob } from '../hooks/useJobs'
import { navigate } from '../hooks/useRoute'
import { startActivity, toast, toastError } from './Toasts'
import { Button, ErrorNote, Field, IconButton, Input, Modal, Spinner } from './ui'

/** Encrypts a folder's files with a password, or decrypts them again. */
export function EncryptDialog({ folder, onClose }: { folder: FileEntry; onClose: () => void }) {
  const [state, setState] = useState<EncryptionState | null>(null)
  const [password, setPassword] = useState('')
  const [again, setAgain] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    toolsApi.encryption(folder.id).then(setState).catch((e) => setError(e instanceof Error ? e.message : String(e)))
  }, [folder.id])

  const first = state ? !state.encrypted : false
  const go = async (lock: boolean) => {
    setBusy(true)
    setError(null)
    try {
      const job = await (lock ? toolsApi.lockFolder(folder.id, password) : toolsApi.unlockFolder(folder.id, password))
      followJob(job.id, `${lock ? 'Encrypting' : 'Decrypting'} ${folder.filename}`)
      onClose()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal open title={`Encryption: ${folder.filename}`} onClose={onClose}
      footer={state && (
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          {state.locked_files > 0 && !first && (
            <Button icon={<LockOpen className="size-3.5" />} loading={busy} disabled={!password} onClick={() => go(false)}>Decrypt</Button>
          )}
          {state.open_files > 0 && (
            <Button variant="primary" icon={<Lock className="size-3.5" />} loading={busy}
              disabled={!password || (first && (password.length < 8 || password !== again))} onClick={() => go(true)}>
              Encrypt
            </Button>
          )}
        </>
      )}>
      {!state ? (error ? <ErrorNote>{error}</ErrorNote> : <Spinner />) : (
        <div className="space-y-3 text-[13px]">
          <p>
            {state.encrypted
              ? `${state.locked_files} file${state.locked_files === 1 ? ' is' : 's are'} encrypted${state.open_files ? `, ${state.open_files} not (yet)` : ''}.`
              : `Encrypting keeps the ${state.open_files} file${state.open_files === 1 ? '' : 's'} in this folder unreadable on the disk until you decrypt them with the password. Names stay visible.`}
          </p>
          {first && (
            <p className="text-warn">
              Without the password the files cannot be recovered by anyone, including HearthDrive. Write it down somewhere safe.
            </p>
          )}
          <Field label="Password">
            <Input type="password" autoFocus value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" />
          </Field>
          {first && (
            <Field label="Password again" hint="At least 8 characters.">
              <Input type="password" value={again} onChange={(e) => setAgain(e.target.value)} autoComplete="new-password" />
            </Field>
          )}
          {error && <ErrorNote>{error}</ErrorNote>}
        </div>
      )}
    </Modal>
  )
}

/** Plugin commands that apply to a file or folder. */
export function PluginsButton({ entry }: { entry: FileEntry }) {
  const [open, setOpen] = useState(false)
  const [commands, setCommands] = useState<PluginCommand[] | null>(null)
  const [running, setRunning] = useState<string | null>(null)
  const [result, setResult] = useState<{ label: string; run: PluginRun } | null>(null)
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    setCommands(null)
    setOpen(false)
  }, [entry.id])
  useEffect(() => {
    if (!open) return
    if (commands === null) toolsApi.pluginCommands(entry.id).then(setCommands).catch(() => setCommands([]))
    const close = (e: MouseEvent) => { if (!ref.current?.contains(e.target as Node)) setOpen(false) }
    document.addEventListener('mousedown', close)
    return () => document.removeEventListener('mousedown', close)
  }, [open, commands, entry.id])

  const run = async (c: PluginCommand) => {
    setOpen(false)
    setRunning(c.label)
    const card = startActivity(`${c.label}: ${entry.filename}`)
    try {
      const r = await toolsApi.runPlugin(c, entry.id)
      if (r.exit_code === 0 && !r.output.trim()) toast(`${c.label}: done`)
      else setResult({ label: c.label, run: r })
    } catch (e) {
      toastError(e)
    } finally {
      card.end()
      setRunning(null)
    }
  }

  return (
    <div className="relative" ref={ref}>
      <IconButton label={running ? `Running ${running}…` : 'Plugins'} onClick={() => setOpen(!open)} disabled={!!running}>
        {running ? <Spinner /> : <Puzzle className="size-4" />}
      </IconButton>
      {open && (
        <div className="absolute right-0 z-30 mt-1 w-64 rounded-md border border-line bg-elevated py-1 shadow-lg">
          {commands === null ? <div className="p-3"><Spinner /></div> : commands.length === 0 ? (
            <p className="px-3 py-2 text-[12.5px] text-muted">
              No plugin works on this.{' '}
              <button className="text-accent hover:underline" onClick={() => navigate('plugins')}>Find plugins</button>
            </p>
          ) : commands.map((c) => (
            <button key={`${c.plugin}/${c.id}`} className="block w-full px-3 py-1.5 text-left text-[13px] hover:bg-hover" onClick={() => void run(c)}>
              {c.label}
              <span className="block text-[11.5px] text-subtle">{c.description || c.plugin_name}</span>
            </button>
          ))}
        </div>
      )}
      {result && (
        <Modal open wide title={result.label} onClose={() => setResult(null)} footer={<Button onClick={() => setResult(null)}>Close</Button>}>
          <p className="mb-2 text-[12.5px] text-muted">
            {result.run.timed_out ? 'It took too long and was stopped.' : result.run.exit_code === 0 ? 'Finished.' : `Finished with code ${result.run.exit_code ?? '?'}.`}
            {' '}{(result.run.took_ms / 1000).toFixed(1)} s
          </p>
          <pre className="max-h-[50vh] overflow-auto rounded-md border border-line bg-surface p-3 text-[12px] whitespace-pre-wrap">{result.run.output || '(no output)'}</pre>
        </Modal>
      )}
    </div>
  )
}
