import { useEffect, useState } from 'react'
import { Home, Pencil, Plus, Trash2, Users } from 'lucide-react'
import { api, fileApi, type FolderAccess, type PeopleGroup, type Person, type StorageRoot } from '../services/api'
import { useResource } from '../hooks/useResource'
import { ACCESS } from './FolderSharing'
import { toast, toastError } from './Toasts'
import { Button, ErrorNote, Field, IconButton, Input, Modal, Spinner } from './ui'

const heading = 'mb-3 text-[11px] font-semibold uppercase tracking-wider text-subtle'
const selectClass = 'h-9 w-full rounded-md border border-line bg-bg px-2 text-[13px] text-fg outline-none focus:border-accent'

/** Ticks people on this computer. */
function PeoplePicker({ people, value, onChange }: { people: Person[]; value: string[]; onChange: (ids: string[]) => void }) {
  if (people.length === 0) return <p className="text-[12.5px] text-subtle">Add people in Settings → People first.</p>
  return (
    <div className="max-h-56 space-y-1 overflow-auto rounded-md border border-line p-2">
      {people.map((p) => (
        <label key={p.id} className="flex items-center gap-2 rounded px-1.5 py-1 hover:bg-hover">
          <input
            type="checkbox"
            className="accent-[var(--nx-accent)]"
            checked={value.includes(p.id)}
            onChange={(e) => onChange(e.target.checked ? [...value, p.id] : value.filter((id) => id !== p.id))}
          />
          <span>{p.username}</span>
          <span className="text-[11.5px] text-subtle">{p.role}</span>
        </label>
      ))}
    </div>
  )
}

function GroupEditor({ group, people, onClose, onSaved }: {
  group: PeopleGroup | null
  people: Person[]
  onClose: () => void
  onSaved: () => void
}) {
  const [name, setName] = useState(group?.name ?? '')
  const [members, setMembers] = useState<string[]>(group?.members.map((m) => m.user_id) ?? [])
  const [busy, setBusy] = useState<'save' | 'remove' | null>(null)
  const [error, setError] = useState('')

  const save = async () => {
    setBusy('save')
    setError('')
    try {
      if (group) await api.updateGroup(group.id, { name, members })
      else await api.createGroup({ name, members })
      toast(`Saved the group ${name.trim()}`)
      onSaved()
      onClose()
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(null)
    }
  }
  const remove = async () => {
    if (!group || !confirm(`Remove the group “${group.name}”? Folders shared with it stop being shared with its people (the files stay).`)) return
    setBusy('remove')
    try {
      await api.removeGroup(group.id)
      toast(`Removed the group ${group.name}`)
      onSaved()
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
      title={group ? `Group: ${group.name}` : 'New group'}
      footer={
        <>
          {group && (
            <Button variant="ghost" className="mr-auto hover:text-danger" loading={busy === 'remove'} onClick={() => void remove()}>
              Remove group
            </Button>
          )}
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy === 'save'} disabled={!name.trim()} onClick={() => void save()}>Save</Button>
        </>
      }
    >
      <div className="space-y-4">
        <Field label="Name">
          <Input autoFocus value={name} placeholder="Family" onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label="People" hint="Folders shared with the group are shared with everyone in it, including people added later.">
          <PeoplePicker people={people} value={members} onChange={setMembers} />
        </Field>
        {error && <ErrorNote>{error}</ErrorNote>}
      </div>
    </Modal>
  )
}

/**
 * A family space: a new folder shared with a group, so everyone in it can
 * keep things together. One step for what Files and Sharing do separately.
 */
function NewSpace({ groups, people, onClose, onSaved }: {
  groups: PeopleGroup[]
  people: Person[]
  onClose: () => void
  onSaved: () => void
}) {
  const [roots, setRoots] = useState<StorageRoot[] | null>(null)
  const [name, setName] = useState('Family')
  const [rootId, setRootId] = useState('')
  const [groupId, setGroupId] = useState(groups[0]?.id ?? 'new')
  const [members, setMembers] = useState<string[]>(people.map((p) => p.id))
  const [access, setAccess] = useState<FolderAccess>('edit')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  useEffect(() => {
    fileApi().roots().then((all) => {
      const usable = all.filter((r) => r.online && !r.read_only)
      setRoots(usable)
      setRootId(usable[0]?.id ?? '')
    }).catch((e) => setError((e as Error).message))
  }, [])

  const create = async () => {
    setBusy(true)
    setError('')
    const folderName = name.trim()
    try {
      const group = groupId === 'new' ? await api.createGroup({ name: folderName, members }) : groups.find((g) => g.id === groupId)!
      const made = await fileApi().createFolder(rootId, '', folderName)
      const path = made.relative_path ?? folderName
      await api.shareFolder({ root_id: rootId, path, name: folderName, members: [{ member: `group:${group.id}`, access }] })
      toast(`Made ${folderName}, shared with ${group.name}`)
      onSaved()
      onClose()
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      open
      onClose={onClose}
      title="New family space"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!name.trim() || !rootId || (groupId === 'new' && members.length === 0)} onClick={() => void create()}>
            Make it
          </Button>
        </>
      }
    >
      {!roots ? (error ? <ErrorNote>{error}</ErrorNote> : <Spinner />) : (
        <div className="space-y-4">
          <p className="text-[12.5px] text-muted">
            A new folder everyone in a group can open and add to, from their own sign-in on this computer. People added to the group
            later get it too.
          </p>
          <Field label="Name">
            <Input autoFocus value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label="On">
            {roots.length === 0 ? <p className="text-[12.5px] text-warn">There is no storage location here to put it on.</p> : (
              <select className={selectClass} value={rootId} onChange={(e) => setRootId(e.target.value)} aria-label="Storage location">
                {roots.map((r) => <option key={r.id} value={r.id}>{r.name}</option>)}
              </select>
            )}
          </Field>
          <Field label="Shared with">
            <select className={selectClass} value={groupId} onChange={(e) => setGroupId(e.target.value)} aria-label="Group">
              {groups.map((g) => <option key={g.id} value={g.id}>{g.name} ({g.members.length} {g.members.length === 1 ? 'person' : 'people'})</option>)}
              <option value="new">A new group with these people…</option>
            </select>
          </Field>
          {groupId === 'new' && <PeoplePicker people={people} value={members} onChange={setMembers} />}
          <Field label="They may">
            <select className={selectClass} value={access} onChange={(e) => setAccess(e.target.value as FolderAccess)} aria-label="What they may do">
              {(Object.keys(ACCESS) as FolderAccess[]).map((a) => <option key={a} value={a}>{ACCESS[a].label}: {ACCESS[a].hint.toLowerCase()}</option>)}
            </select>
          </Field>
          {error && <ErrorNote>{error}</ErrorNote>}
        </div>
      )}
    </Modal>
  )
}

/** Groups of people, and family spaces made from them (owners and administrators). */
export function GroupsSection({ onChanged }: { onChanged: () => void }) {
  const groups = useResource(() => api.groups(), [])
  const people = useResource(() => api.users(), [])
  const [editing, setEditing] = useState<PeopleGroup | 'new' | null>(null)
  const [space, setSpace] = useState(false)
  const reload = () => {
    groups.reload()
    onChanged()
  }
  const list = groups.data ?? []
  return (
    <section>
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <h2 className={`${heading} mb-0 mr-auto`}>Groups</h2>
        <Button size="sm" icon={<Plus className="size-3.5" />} onClick={() => setEditing('new')}>New group</Button>
        <Button size="sm" variant="primary" icon={<Home className="size-3.5" />} disabled={!groups.data || !people.data} onClick={() => setSpace(true)}>
          New family space
        </Button>
      </div>
      {groups.error ? <ErrorNote>{groups.error.message}</ErrorNote> : !groups.data ? <Spinner /> : list.length === 0 ? (
        <p className="px-1 text-[12.5px] text-subtle">
          A group (your family, say) lets you share folders with all of its people at once. New family space makes a folder and shares
          it with a group in one go.
        </p>
      ) : (
        <ul className="divide-y divide-line rounded-lg border border-line bg-panel">
          {list.map((g) => (
            <li key={g.id} className="flex items-center gap-3 px-4 py-3 md:px-5">
              <Users className="size-4 shrink-0 text-accent" />
              <div className="min-w-0 flex-1">
                <div className="truncate font-medium">{g.name}</div>
                <div className="mt-0.5 truncate text-[12px] text-subtle">
                  {g.members.length ? g.members.map((m) => m.username).join(', ') : 'Nobody yet'}
                  {g.folders.length > 0 && ` · shares ${g.folders.join(', ')}`}
                </div>
              </div>
              <IconButton label={`Edit ${g.name}`} onClick={() => setEditing(g)}><Pencil className="size-4" /></IconButton>
              <IconButton
                label={`Remove ${g.name}`}
                onClick={async () => {
                  if (!confirm(`Remove the group “${g.name}”? Folders shared with it stop being shared with its people (the files stay).`)) return
                  try {
                    await api.removeGroup(g.id)
                    reload()
                  } catch (e) {
                    toastError(e)
                  }
                }}
              >
                <Trash2 className="size-4" />
              </IconButton>
            </li>
          ))}
        </ul>
      )}
      {editing && people.data && (
        <GroupEditor group={editing === 'new' ? null : editing} people={people.data} onClose={() => setEditing(null)} onSaved={reload} />
      )}
      {space && groups.data && people.data && (
        <NewSpace groups={groups.data} people={people.data} onClose={() => setSpace(false)} onSaved={reload} />
      )}
    </section>
  )
}
