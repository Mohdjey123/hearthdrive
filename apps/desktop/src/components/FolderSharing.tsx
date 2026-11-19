import { useEffect, useState } from 'react'
import { MessageSquare, Monitor, Trash2, User, UserPlus, Users, X } from 'lucide-react'
import {
  api, type Device, type FileApi, type FileComment, type FileEntry, type FolderAccess, type PeopleGroup, type Person, type SharedFolder,
} from '../services/api'
import { formatRelative } from '../lib/format'
import { toast, toastError } from './Toasts'
import { Badge, Button, ErrorNote, Field, IconButton, Input, Modal, Spinner } from './ui'

export const ACCESS: Record<FolderAccess, { label: string; hint: string }> = {
  view: { label: 'Can view', hint: 'Open and download' },
  edit: { label: 'Can edit', hint: 'Add, change and delete' },
  add: { label: 'Add files only', hint: 'A drop folder: what is inside stays hidden' },
}

export function AccessBadge({ access }: { access: FolderAccess }) {
  return <Badge tone={access === 'edit' ? 'accent' : 'neutral'}>{ACCESS[access].label}</Badge>
}

const selectClass = 'h-8 rounded-md border border-line bg-bg px-2 text-[12.5px] text-fg outline-none focus:border-accent'

function AccessSelect({ value, onChange, label }: { value: FolderAccess; onChange: (a: FolderAccess) => void; label: string }) {
  return (
    <select aria-label={label} value={value} onChange={(e) => onChange(e.target.value as FolderAccess)} className={selectClass}>
      {(Object.keys(ACCESS) as FolderAccess[]).map((a) => <option key={a} value={a}>{ACCESS[a].label}</option>)}
    </select>
  )
}

interface Candidate {
  member: string
  name: string
  kind: 'person' | 'device' | 'group'
}

interface Row extends Candidate {
  access: FolderAccess
  note?: string | null
}

/** The folder being shared: a new one, or one shared already. */
export interface ShareTarget {
  root_id: string
  path: string
  name: string
}

const samePlace = (f: SharedFolder, t: ShareTarget) => f.root.id === t.root_id && f.path.toLowerCase() === t.path.toLowerCase()

/**
 * Chooses who a folder is shared with and what they may do. People who see
 * the whole storage location already (owners, administrators, anyone not
 * limited) gain nothing, so the list offers the others first.
 */
export function ShareFolderDialog({ target, onClose, onSaved }: {
  target: ShareTarget
  onClose: () => void
  onSaved?: () => void
}) {
  const [existing, setExisting] = useState<SharedFolder | null | undefined>(undefined)
  const [candidates, setCandidates] = useState<Candidate[]>([])
  const [rows, setRows] = useState<Row[]>([])
  const [name, setName] = useState(target.name)
  const [adding, setAdding] = useState('')
  const [addAccess, setAddAccess] = useState<FolderAccess>('view')
  const [busy, setBusy] = useState<'save' | 'stop' | null>(null)
  const [error, setError] = useState('')

  useEffect(() => {
    let live = true
    Promise.all([api.sharedFolders(), api.users(), api.devices(), api.groups().catch(() => [] as PeopleGroup[])])
      .then(([folders, people, devices, groups]: [SharedFolder[], Person[], Device[], PeopleGroup[]]) => {
        if (!live) return
        const found = folders.find((f) => samePlace(f, target)) ?? null
        setExisting(found)
        if (found) {
          setName(found.name)
          setRows(found.members.map((m) => ({ member: m.member, name: m.name, kind: m.kind, access: m.access, note: m.note })))
        }
        const sorted = [
          ...groups.map((g): Candidate => ({ member: `group:${g.id}`, name: g.name, kind: 'group' })),
          ...people
            .filter((p) => p.role !== 'owner' && p.role !== 'administrator')
            .map((p): Candidate => ({ member: `user:${p.id}`, name: p.username, kind: 'person' })),
          ...devices.filter((d) => !d.is_local).map((d): Candidate => ({ member: `device:${d.id}`, name: d.name, kind: 'device' })),
        ]
        setCandidates(sorted)
      })
      .catch((e) => live && setError((e as Error).message))
    return () => { live = false }
  }, [target.root_id, target.path])

  const free = candidates.filter((c) => !rows.some((r) => r.member === c.member))
  useEffect(() => {
    if (!free.some((c) => c.member === adding)) setAdding(free[0]?.member ?? '')
  }, [free.map((c) => c.member).join(), adding])

  const add = () => {
    const c = free.find((x) => x.member === adding)
    if (c) setRows((r) => [...r, { ...c, access: addAccess }])
  }

  const save = async () => {
    setBusy('save')
    setError('')
    const members = rows.map((r) => ({ member: r.member, access: r.access }))
    try {
      if (existing) await api.updateSharedFolder(existing.id, { name, members })
      else await api.shareFolder({ root_id: target.root_id, path: target.path, name, members })
      toast(rows.length ? `Shared ${name} with ${rows.length} ${rows.length === 1 ? 'member' : 'members'}` : `Saved ${name}`)
      onSaved?.()
      onClose()
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(null)
    }
  }

  const stop = async () => {
    if (!existing) return
    setBusy('stop')
    try {
      await api.unshareFolder(existing.id)
      toast(`Stopped sharing ${existing.name}`)
      onSaved?.()
      onClose()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(null)
    }
  }

  return (
    <Modal
      open
      onClose={onClose}
      title={`Share ${target.name} with people`}
      footer={
        <>
          {existing && (
            <Button variant="ghost" className="mr-auto hover:text-danger" loading={busy === 'stop'} onClick={() => void stop()}>
              Stop sharing
            </Button>
          )}
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy === 'save'} disabled={existing === undefined} onClick={() => void save()}>
            {existing ? 'Save' : 'Share'}
          </Button>
        </>
      }
    >
      {existing === undefined && !error ? (
        <Spinner />
      ) : (
        <div className="space-y-4">
          <Field label="Name" hint="What the people it is shared with see.">
            <Input value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <div>
            <div className="mb-1.5 text-[12.5px] font-medium">Shared with</div>
            {rows.length === 0 ? (
              <p className="text-[12.5px] text-subtle">No one yet.</p>
            ) : (
              <ul className="divide-y divide-line rounded-md border border-line">
                {rows.map((r) => (
                  <li key={r.member} className="flex flex-wrap items-center gap-2 px-3 py-2">
                    {r.kind === 'device' ? <Monitor className="size-4 shrink-0 text-subtle" />
                      : r.kind === 'group' ? <Users className="size-4 shrink-0 text-subtle" />
                      : <User className="size-4 shrink-0 text-subtle" />}
                    <div className="min-w-0 flex-1">
                      <div className="truncate">{r.name}</div>
                      {r.note && <div className={r.kind === 'group' ? 'text-[11.5px] text-subtle' : 'text-[11.5px] text-warn'}>{r.note}</div>}
                    </div>
                    <AccessSelect
                      label={`What ${r.name} may do`}
                      value={r.access}
                      onChange={(access) => setRows((all) => all.map((x) => (x.member === r.member ? { ...x, access, note: undefined } : x)))}
                    />
                    <IconButton label={`Remove ${r.name}`} onClick={() => setRows((all) => all.filter((x) => x.member !== r.member))}>
                      <X className="size-4" />
                    </IconButton>
                  </li>
                ))}
              </ul>
            )}
          </div>
          {free.length > 0 ? (
            <div className="flex flex-wrap items-center gap-2">
              <select aria-label="Person, group or device" value={adding} onChange={(e) => setAdding(e.target.value)} className={`${selectClass} min-w-0 flex-1`}>
                {free.map((c) => (
                  <option key={c.member} value={c.member}>
                    {c.kind === 'device' ? `${c.name} (device)` : c.kind === 'group' ? `${c.name} (group)` : c.name}
                  </option>
                ))}
              </select>
              <AccessSelect label="What they may do" value={addAccess} onChange={setAddAccess} />
              <Button size="sm" icon={<UserPlus className="size-3.5" />} onClick={add}>Add</Button>
            </div>
          ) : (
            candidates.length === 0 && (
              <p className="text-[12.5px] text-subtle">
                Add people in Settings → People (limited to some storage), make a group on Sharing, or pair devices to share folders with them.
              </p>
            )
          )}
          <p className="text-[12px] text-subtle">
            {ACCESS[addAccess].label}: {ACCESS[addAccess].hint.toLowerCase()}. Sharing never goes beyond someone’s role: a viewer
            given “Can edit” still only views. Owners and administrators see everything already.
          </p>
          {error && <ErrorNote>{error}</ErrorNote>}
        </div>
      )}
    </Modal>
  )
}

/** Comments on a file, for everyone who may open it. */
export function Comments({ file, fapi }: { file: FileEntry; fapi: FileApi }) {
  const [comments, setComments] = useState<FileComment[] | null>(null)
  const [error, setError] = useState('')
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    let live = true
    setComments(null)
    setError('')
    fapi
      .comments(file.id)
      .then((c) => live && setComments(c))
      .catch((e) => live && setError((e as Error).message))
    return () => { live = false }
  }, [file.id, fapi])

  const post = async () => {
    const body = text.trim()
    if (!body) return
    setBusy(true)
    try {
      const c = await fapi.addComment(file.id, body)
      setComments((all) => [...(all ?? []), c])
      setText('')
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }

  const remove = async (c: FileComment) => {
    try {
      await fapi.deleteComment(file.id, c.id)
      setComments((all) => (all ?? []).filter((x) => x.id !== c.id))
    } catch (e) {
      toastError(e)
    }
  }

  return (
    <section className="mt-6">
      <h3 className="mb-2 flex items-center gap-1.5 text-[12px] font-semibold tracking-wide text-muted uppercase">
        <MessageSquare className="size-3.5" /> Comments
      </h3>
      {error ? (
        <p className="text-[12px] text-subtle">{error}</p>
      ) : !comments ? (
        <Spinner />
      ) : (
        <>
          {comments.length > 0 && (
            <ul className="mb-3 space-y-3">
              {comments.map((c) => (
                <li key={c.id} className="group">
                  <div className="flex items-center gap-1.5 text-[11.5px] text-subtle">
                    <span className="font-medium text-fg">{c.author}</span>
                    <span>{formatRelative(c.created_at)}</span>
                    {c.can_delete && (
                      <button
                        className="ml-auto opacity-0 group-hover:opacity-100 focus:opacity-100 hover:text-danger"
                        aria-label="Delete comment"
                        onClick={() => void remove(c)}
                      >
                        <Trash2 className="size-3.5" />
                      </button>
                    )}
                  </div>
                  <p className="mt-0.5 break-words whitespace-pre-wrap">{c.body}</p>
                </li>
              ))}
            </ul>
          )}
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) void post()
            }}
            maxLength={4000}
            rows={2}
            placeholder="Add a comment"
            className="w-full resize-y rounded-md border border-line bg-bg px-2.5 py-1.5 text-[13px] text-fg outline-none placeholder:text-subtle focus:border-accent"
          />
          <div className="mt-1.5 flex justify-end">
            <Button size="sm" loading={busy} disabled={!text.trim()} onClick={() => void post()}>Comment</Button>
          </div>
        </>
      )}
    </section>
  )
}
