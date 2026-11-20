import { useEffect, useMemo, useState } from 'react'
import { ArrowRight, Eye, Lightbulb, Pencil, Play, Plus, Trash2, Wand2 } from 'lucide-react'
import {
  api, fileApi, libraryApi, peerPrefix, rulesApi,
  type Device, type Rule, type RuleAction, type RuleConditions, type RuleDraft, type RulePreviewItem, type RuleTrigger,
  type StorageRoot, type StorageRoom, type StorageSuggestion,
} from '../services/api'
import { useResource } from '../hooks/useResource'
import { followJob, useJobFor } from '../hooks/useJobs'
import { JobProgress } from '../components/JobProgress'
import { navigate } from '../hooks/useRoute'
import { formatBytes, formatRelative } from '../lib/format'
import { FileIcon } from '../components/FileIcon'
import { FolderPicker } from '../components/FilePreview'
import { toast, toastError } from '../components/Toasts'
import { Badge, Button, EmptyState, ErrorNote, Field, IconButton, Input, Modal, PageHeader, Panel, Progress, Spinner, cx } from '../components/ui'

const SELECT = 'h-8 w-full rounded-md border border-line bg-panel px-2 text-[13px]'
const KINDS: [string, string][] = [
  ['image', 'Photos'], ['video', 'Videos'], ['audio', 'Music'], ['document', 'Documents'], ['archive', 'Archives'], ['code', 'Code'],
]
const TRIGGERS: Record<RuleTrigger, string> = {
  added: 'When files arrive',
  hourly: 'Every hour',
  daily: 'Every day',
}

/** Ready-made rules to start from. */
function templates(rootId: string): { label: string; draft: RuleDraft }[] {
  return [
    {
      label: 'Sort invoices into Documents/Invoices',
      draft: { name: 'Sort invoices', trigger: 'added', root_id: rootId, path: '', when: { subfolders: true, tag: 'invoice' }, then: { type: 'move', folder: 'Documents/Invoices' } },
    },
    {
      label: 'File photos by year and month',
      draft: { name: 'Photos by date', trigger: 'added', root_id: rootId, path: 'Downloads', when: { kinds: ['image'] }, then: { type: 'move', folder: 'Photos/{year}/{month}' } },
    },
    {
      label: 'Clear out old downloads',
      draft: { name: 'Old downloads', trigger: 'daily', root_id: rootId, path: 'Downloads', when: { subfolders: true, older_than_days: 90 }, then: { type: 'trash' } },
    },
    {
      label: 'Copy new videos to another device',
      draft: { name: 'Copy videos', trigger: 'added', root_id: rootId, path: '', when: { subfolders: true, kinds: ['video'] }, then: { type: 'copy', device_id: null, root_id: '', folder: '' } },
    },
    {
      label: 'Tag big files',
      draft: { name: 'Tag big files', trigger: 'daily', root_id: rootId, path: '', when: { subfolders: true, larger_than_mb: 1024 }, then: { type: 'tag', tags: ['big'] } },
    },
  ]
}

function describe(rule: { when: RuleConditions | null; then: RuleAction | null; path: string; root_name?: string | null }) {
  const w = rule.when ?? {}
  const parts: string[] = []
  if (w.kinds?.length) parts.push(w.kinds.map((k) => KINDS.find(([v]) => v === k)?.[1].toLowerCase() ?? k).join(' or '))
  if (w.extensions?.length) parts.push(w.extensions.map((e) => `.${e}`).join(', '))
  if (w.tag) parts.push(`tagged “${w.tag}”`)
  if (w.name_contains) parts.push(`named “${w.name_contains}”`)
  if (w.text_contains) parts.push(`mentioning “${w.text_contains}”`)
  if (w.older_than_days) parts.push(`unchanged for ${w.older_than_days} days`)
  if (w.larger_than_mb) parts.push(`over ${w.larger_than_mb} MB`)
  if (w.smaller_than_mb) parts.push(`under ${w.smaller_than_mb} MB`)
  const what = parts.length ? `Files ${parts.join(', ')}` : 'All files'
  const where = `in ${rule.root_name ?? 'storage'}${rule.path ? `/${rule.path}` : ''}${w.subfolders ? ' and its subfolders' : ''}`
  const a = rule.then
  const action = !a ? '' : a.type === 'tag' ? `get tagged ${a.tags.map((t) => `“${t}”`).join(', ')}`
    : a.type === 'move' ? `move to ${a.folder || 'the top folder'}`
    : a.type === 'copy' ? `are copied${a.device_id ? ' to another device' : ''}${a.folder ? ` into ${a.folder}` : ''}`
    : 'go to the trash'
  return `${what} ${where} ${action}.`
}

/** Rules that tidy files by themselves, and suggestions for where things could go. */
/** A rule running now: its files counted as they are done. */
function RuleProgress({ rule }: { rule: Rule }) {
  const job = useJobFor(rule.id)
  return job ? <JobProgress job={job} label="Running" className="mt-2" /> : <Progress label="Running" className="mt-2" />
}

export function Automation() {
  const rules = useResource(() => rulesApi.list(), [])
  const roots = useResource(() => fileApi().roots(), [])
  const devices = useResource(() => api.devices().catch(() => [] as Device[]), [])
  const suggestions = useResource(() => rulesApi.suggestions(), [])
  const [editing, setEditing] = useState<{ id?: string; draft: RuleDraft } | null>(null)
  const [previewing, setPreviewing] = useState<Rule | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [dismissed, setDismissed] = useState<Set<string>>(() => {
    try { return new Set(JSON.parse(localStorage.getItem('nexus.suggestions.dismissed') ?? '[]')) } catch { return new Set() }
  })
  const local = (roots.data ?? []).filter((r) => r.online && !r.read_only)
  const firstRoot = local[0]?.id ?? ''

  const dismiss = (id: string) => {
    const next = new Set(dismissed).add(id)
    setDismissed(next)
    try { localStorage.setItem('nexus.suggestions.dismissed', JSON.stringify([...next])) } catch { /* not kept */ }
  }

  const runNow = async (rule: Rule) => {
    setBusy(rule.id)
    try {
      const r = await rulesApi.run(rule.id)
      toast(r.matched === 0 ? `${rule.name}: nothing to do` : `${rule.name}: ${r.done} of ${r.matched} files done${r.failed ? `, ${r.failed} failed` : ''}`, r.failed ? 'error' : 'success')
      if (r.job_id) followJob(r.job_id, `${rule.name}: copying`)
      rules.reload()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(null)
    }
  }

  const toggle = async (rule: Rule) => {
    if (!rule.when || !rule.then) return
    try {
      await rulesApi.update(rule.id, { name: rule.name, enabled: !rule.enabled, trigger: rule.trigger, root_id: rule.root_id, path: rule.path, when: rule.when, then: rule.then })
      rules.reload()
    } catch (e) {
      toastError(e)
    }
  }

  const remove = async (rule: Rule) => {
    if (!confirm(`Delete the rule “${rule.name}”? Files it already dealt with stay where they are.`)) return
    try {
      await rulesApi.remove(rule.id)
      rules.reload()
    } catch (e) {
      toastError(e)
    }
  }

  const shown = (suggestions.data ?? []).filter((s) => !dismissed.has(s.id))
  return (
    <div className="space-y-5">
      <PageHeader
        title="Automation"
        subtitle="Rules tidy files by themselves: sort paperwork, file photos by date, clear out old downloads or copy new files to another device."
        actions={
          <Button variant="primary" icon={<Plus className="size-3.5" />} disabled={!firstRoot}
            onClick={() => setEditing({ draft: { name: '', trigger: 'added', root_id: firstRoot, path: '', when: { subfolders: true }, then: { type: 'move', folder: '' } } })}>
            New rule
          </Button>
        }
      />

      {shown.length > 0 && (
        <Panel title={<span className="flex items-center gap-1.5"><Lightbulb className="size-3.5" /> Suggestions</span>} padded={false}>
          <ul className="divide-y divide-line">
            {shown.map((s) => (
              <SuggestionRow key={s.id} s={s} devices={devices.data ?? []}
                onRule={(draft) => setEditing({ draft })}
                onDone={() => { dismiss(s.id); suggestions.reload() }}
                onDismiss={() => dismiss(s.id)} />
            ))}
          </ul>
        </Panel>
      )}

      <Panel title="Rules" padded={false}>
        {rules.error ? (
          <div className="p-4"><ErrorNote>{rules.error.message}</ErrorNote></div>
        ) : !rules.data ? (
          <div className="p-4"><Spinner /></div>
        ) : rules.data.length === 0 ? (
          <div className="p-4">
            <EmptyState icon={<Wand2 className="size-5" />} title="No rules yet">
              Start from one of these, or make your own with New rule.
            </EmptyState>
            <div className="mt-3 flex flex-wrap justify-center gap-2">
              {templates(firstRoot).map((t) => (
                <Button key={t.label} size="sm" disabled={!firstRoot} onClick={() => setEditing({ draft: t.draft })}>{t.label}</Button>
              ))}
            </div>
          </div>
        ) : (
          <ul className="divide-y divide-line">
            {rules.data.map((rule) => (
              <li key={rule.id} className={cx('flex items-start gap-3 px-4 py-3', !rule.enabled && 'opacity-60')}>
                <input type="checkbox" className="mt-1" checked={rule.enabled} onChange={() => toggle(rule)} aria-label={rule.enabled ? 'Turn off' : 'Turn on'} />
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-medium">{rule.name}</span>
                    <Badge>{TRIGGERS[rule.trigger]}</Badge>
                    {rule.files_done > 0 && <Badge tone="accent">{rule.files_done} files so far</Badge>}
                  </div>
                  <p className="mt-0.5 text-[12.5px] text-muted">{describe(rule)}</p>
                  {rule.last_run && busy !== rule.id && (
                    <p className="mt-0.5 text-[12px] text-subtle">Last ran {formatRelative(rule.last_run)}: {rule.last_result}</p>
                  )}
                  {busy === rule.id && <RuleProgress rule={rule} />}
                </div>
                <div className="flex shrink-0 items-center gap-1">
                  <IconButton label="See what it would do" onClick={() => setPreviewing(rule)}><Eye className="size-4" /></IconButton>
                  <IconButton label="Run now" disabled={busy === rule.id} onClick={() => runNow(rule)}>
                    {busy === rule.id ? <Spinner /> : <Play className="size-4" />}
                  </IconButton>
                  <IconButton label="Edit" disabled={!rule.when || !rule.then}
                    onClick={() => rule.when && rule.then && setEditing({ id: rule.id, draft: { name: rule.name, enabled: rule.enabled, trigger: rule.trigger, root_id: rule.root_id, path: rule.path, when: rule.when, then: rule.then } })}>
                    <Pencil className="size-4" />
                  </IconButton>
                  <IconButton label="Delete" onClick={() => remove(rule)}><Trash2 className="size-4" /></IconButton>
                </div>
              </li>
            ))}
          </ul>
        )}
      </Panel>
      {rules.data && rules.data.length > 0 && (
        <div className="flex flex-wrap gap-2">
          <span className="self-center text-[12px] text-subtle">Start from:</span>
          {templates(firstRoot).map((t) => (
            <Button key={t.label} size="sm" variant="ghost" disabled={!firstRoot} onClick={() => setEditing({ draft: t.draft })}>{t.label}</Button>
          ))}
        </div>
      )}

      {editing && (
        <RuleEditor initial={editing.draft} id={editing.id} roots={local} devices={devices.data ?? []}
          onClose={() => setEditing(null)}
          onSaved={() => { setEditing(null); rules.reload(); suggestions.reload() }} />
      )}
      {previewing && (
        <PreviewModal title={`What “${previewing.name}” would do now`} load={() => rulesApi.preview(previewing.id)} onClose={() => setPreviewing(null)} />
      )}
    </div>
  )
}

function SuggestionRow({ s, devices, onRule, onDone, onDismiss }: {
  s: StorageSuggestion
  devices: Device[]
  onRule: (d: RuleDraft) => void
  onDone: () => void
  onDismiss: () => void
}) {
  const [moving, setMoving] = useState(false)
  const [open, setOpen] = useState(false)
  return (
    <li className="px-4 py-3">
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium">{s.title}</span>
            {s.bytes > 0 && <Badge tone={s.kind === 'full' ? 'warn' : 'neutral'}>{formatBytes(s.bytes)}</Badge>}
          </div>
          <p className="mt-0.5 text-[12.5px] text-muted">{s.detail}</p>
          {s.files.length > 0 && (
            <button className="mt-1 text-[12px] text-accent hover:underline" onClick={() => setOpen(!open)}>
              {open ? 'Hide' : 'Show'} the {s.files.length} files
            </button>
          )}
          {open && (
            <ul className="mt-2 max-h-48 space-y-1 overflow-auto text-[12.5px]">
              {s.files.map((f) => (
                <li key={f.id} className="flex items-center gap-2">
                  <FileIcon entry={f} className="size-4" />
                  <span className="min-w-0 flex-1 truncate">{f.relative_path}</span>
                  <span className="text-subtle">{formatBytes(f.size)}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {s.kind === 'duplicates' && <Button size="sm" onClick={() => navigate('duplicates')}>Review</Button>}
          {s.kind === 'sort' && s.rule && <Button size="sm" onClick={() => onRule(s.rule!)}>Make the rule</Button>}
          {s.kind === 'full' && s.room_on.length > 0 && <Button size="sm" onClick={() => setMoving(true)}>Move…</Button>}
          <Button size="sm" variant="ghost" onClick={onDismiss}>Dismiss</Button>
        </div>
      </div>
      {moving && <MoveModal s={s} devices={devices} onClose={() => setMoving(false)} onDone={() => { setMoving(false); onDone() }} />}
    </li>
  )
}

/** Moves the files of a "drive is full" suggestion to storage with room. */
function MoveModal({ s, devices, onClose, onDone }: { s: StorageSuggestion; devices: Device[]; onClose: () => void; onDone: () => void }) {
  const [room, setRoom] = useState<StorageRoom>(s.room_on[0])
  const [peerRoots, setPeerRoots] = useState<StorageRoot[] | null>(null)
  const [rootId, setRootId] = useState(room.root_id ?? '')
  const [busy, setBusy] = useState(false)
  const isPeer = room.root_id === null
  useEffect(() => {
    if (!isPeer) { setRootId(room.root_id ?? ''); return }
    setPeerRoots(null)
    fileApi(peerPrefix(room.device_id)).roots()
      .then((r) => { const ok = r.filter((x) => x.online && !x.read_only); setPeerRoots(ok); setRootId(ok[0]?.id ?? '') })
      .catch(() => setPeerRoots([]))
  }, [room.device_id, room.root_id, isPeer])
  const total = s.files.reduce((n, f) => n + f.size, 0)
  const name = devices.find((d) => d.is_local)?.name ?? 'this computer'

  const go = async () => {
    setBusy(true)
    try {
      const fapi = fileApi(isPeer ? peerPrefix(room.device_id) : '')
      const folder = `Moved from ${name}`
      try { await fapi.createFolder(rootId, '', folder) } catch { /* it may exist already */ }
      const job = await api.copyItems({ file_ids: s.files.map((f) => f.id), to_device: isPeer ? room.device_id : undefined, root_id: rootId, path: folder, remove_originals: true })
      followJob(job.id, `Moving ${s.files.length} files`)
      onDone()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal open title={`Move ${s.files.length} files (${formatBytes(total)})`} onClose={onClose}
      footer={<><Button variant="ghost" onClick={onClose}>Cancel</Button><Button variant="primary" loading={busy} disabled={!rootId} onClick={go}>Move</Button></>}>
      <div className="space-y-3">
        <Field label="To">
          <select className={SELECT} value={s.room_on.indexOf(room)} onChange={(e) => setRoom(s.room_on[Number(e.target.value)])}>
            {s.room_on.map((r, i) => (
              <option key={i} value={i}>
                {r.root_name ? `${r.device_name}: ${r.root_name}` : r.device_name}{r.available_bytes != null ? ` (${formatBytes(r.available_bytes)} free)` : ''}
              </option>
            ))}
          </select>
        </Field>
        {isPeer && (
          <Field label="Storage on that device">
            {peerRoots === null ? <Spinner /> : peerRoots.length === 0 ? <ErrorNote>That device has no storage to put files in.</ErrorNote> : (
              <select className={SELECT} value={rootId} onChange={(e) => setRootId(e.target.value)}>
                {peerRoots.map((r) => <option key={r.id} value={r.id}>{r.name}{r.disk ? ` (${formatBytes(r.disk.available_bytes)} free)` : ''}</option>)}
              </select>
            )}
          </Field>
        )}
        <p className="text-[12.5px] text-muted">
          The files go into a folder “Moved from {name}”. Each original goes to the trash here only once it has been copied without a problem.
        </p>
      </div>
    </Modal>
  )
}

function PreviewModal({ title, load, onClose }: { title: string; load: () => Promise<RulePreviewItem[]>; onClose: () => void }) {
  const items = useResource(load, [])
  return (
    <Modal open wide title={title} onClose={onClose} footer={<Button onClick={onClose}>Close</Button>}>
      {items.error ? <ErrorNote>{items.error.message}</ErrorNote> : !items.data ? <Spinner /> : items.data.length === 0 ? (
        <p className="text-[13px] text-muted">No files match right now. Files the rule already dealt with are not counted again.</p>
      ) : (
        <>
          <p className="mb-2 text-[12.5px] text-muted">{items.data.length === 100 ? 'The first 100 files' : items.data.length === 1 ? 'The one file' : `${items.data.length} files`} it would deal with:</p>
          <ul className="max-h-[50vh] space-y-1 overflow-auto text-[12.5px]">
            {items.data.map(({ file, target }) => (
              <li key={file.id} className="flex items-center gap-2">
                <FileIcon entry={file} className="size-4 shrink-0" />
                <span className="min-w-0 flex-1 truncate">{file.relative_path}</span>
                {target != null && <><ArrowRight className="size-3 shrink-0 text-subtle" /><span className="min-w-0 max-w-[40%] truncate text-muted">{target || 'top folder'}</span></>}
              </li>
            ))}
          </ul>
        </>
      )}
    </Modal>
  )
}

function num(v: string): number | null {
  const n = Number(v)
  return v.trim() && Number.isFinite(n) && n > 0 ? n : null
}

function RuleEditor({ initial, id, roots, devices, onClose, onSaved }: {
  initial: RuleDraft
  id?: string
  roots: StorageRoot[]
  devices: Device[]
  onClose: () => void
  onSaved: () => void
}) {
  const [d, setD] = useState<RuleDraft>(() => ({ ...initial, root_id: initial.root_id || roots[0]?.id || '' }))
  const [browse, setBrowse] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [preview, setPreview] = useState(false)
  const [tags, setTags] = useState<string[]>([])
  const [copyRoots, setCopyRoots] = useState<StorageRoot[] | null>(null)
  const w = d.when
  const a = d.then
  const setWhen = (p: Partial<RuleConditions>) => setD({ ...d, when: { ...w, ...p } })
  const setThen = (t: RuleAction) => setD({ ...d, then: t })
  const peers = devices.filter((x) => !x.is_local)
  const copyDevice = a.type === 'copy' ? a.device_id ?? '' : ''

  useEffect(() => { libraryApi('').tags().then((t) => setTags(t.map((x) => x.tag))).catch(() => {}) }, [])
  useEffect(() => {
    if (a.type !== 'copy') return
    setCopyRoots(null)
    fileApi(copyDevice ? peerPrefix(copyDevice) : '').roots()
      .then((r) => setCopyRoots(r.filter((x) => x.online && !x.read_only)))
      .catch(() => setCopyRoots([]))
  }, [a.type, copyDevice])
  useEffect(() => {
    if (a.type === 'copy' && copyRoots?.length && !copyRoots.some((r) => r.id === a.root_id)) setThen({ ...a, root_id: copyRoots[0].id })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [copyRoots])

  const draft: RuleDraft = useMemo(() => ({ ...d, name: d.name.trim() || 'Untitled rule' }), [d])

  const save = async () => {
    setBusy(true)
    setError(null)
    try {
      if (id) await rulesApi.update(id, draft)
      else await rulesApi.create(draft)
      toast(id ? 'Rule saved' : 'Rule made. Use Run now to apply it to the files already there.')
      onSaved()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal open wide title={id ? 'Edit rule' : 'New rule'} onClose={onClose}
      footer={
        <>
          <Button variant="ghost" icon={<Eye className="size-3.5" />} onClick={() => setPreview(true)}>See what it would do</Button>
          <span className="flex-1" />
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} onClick={save}>Save</Button>
        </>
      }>
      <div className="space-y-4">
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Name"><Input value={d.name} onChange={(e) => setD({ ...d, name: e.target.value })} placeholder="Sort invoices" autoFocus /></Field>
          <Field label="Runs">
            <select className={SELECT} value={d.trigger} onChange={(e) => setD({ ...d, trigger: e.target.value as RuleTrigger })}>
              {Object.entries(TRIGGERS).map(([v, l]) => <option key={v} value={v}>{l}</option>)}
            </select>
          </Field>
        </div>

        <fieldset className="space-y-2 rounded-md border border-line p-3">
          <legend className="px-1 text-[12px] font-semibold text-muted">Looks in</legend>
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Storage">
              <select className={SELECT} value={d.root_id} onChange={(e) => setD({ ...d, root_id: e.target.value, path: '' })}>
                {roots.map((r) => <option key={r.id} value={r.id}>{r.name}</option>)}
              </select>
            </Field>
            <Field label="Folder" hint={<button type="button" className="text-accent hover:underline" onClick={() => setBrowse(!browse)}>{browse ? 'Hide folders' : 'Choose a folder'}</button>}>
              <Input value={d.path} onChange={(e) => setD({ ...d, path: e.target.value })} placeholder="The whole storage" />
            </Field>
          </div>
          {browse && d.root_id && <FolderPicker rootId={d.root_id} value={d.path} onChange={(p) => setD({ ...d, path: p })} />}
          <label className="flex items-center gap-2 text-[13px]">
            <input type="checkbox" checked={!!w.subfolders} onChange={(e) => setWhen({ subfolders: e.target.checked })} /> And its subfolders
          </label>
        </fieldset>

        <fieldset className="space-y-3 rounded-md border border-line p-3">
          <legend className="px-1 text-[12px] font-semibold text-muted">Files that</legend>
          <div className="flex flex-wrap gap-x-4 gap-y-1 text-[13px]">
            {KINDS.map(([v, l]) => (
              <label key={v} className="flex items-center gap-1.5">
                <input type="checkbox" checked={!!w.kinds?.includes(v)}
                  onChange={(e) => setWhen({ kinds: e.target.checked ? [...(w.kinds ?? []), v] : (w.kinds ?? []).filter((k) => k !== v) })} />
                {l}
              </label>
            ))}
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Have these extensions"><Input value={(w.extensions ?? []).join(', ')} placeholder="pdf, docx"
              onChange={(e) => setWhen({ extensions: e.target.value.split(/[\s,]+/).map((x) => x.replace(/^\./, '')).filter(Boolean) })} /></Field>
            <Field label="Have a name containing"><Input value={w.name_contains ?? ''} onChange={(e) => setWhen({ name_contains: e.target.value || null })} placeholder="Screenshot" /></Field>
            <Field label="Mention" hint="Words in the document's text"><Input value={w.text_contains ?? ''} onChange={(e) => setWhen({ text_contains: e.target.value || null })} placeholder="Electricity" /></Field>
            <Field label="Have the tag">
              <Input list="rule-tags" value={w.tag ?? ''} onChange={(e) => setWhen({ tag: e.target.value || null })} placeholder="invoice" />
              <datalist id="rule-tags">{['invoice', 'receipt', 'bank statement', 'tax', 'payslip', 'contract', 'insurance', 'medical', ...tags].filter((t, i, all) => all.indexOf(t) === i).map((t) => <option key={t} value={t} />)}</datalist>
            </Field>
            <Field label="Haven't changed for (days)"><Input type="number" min={1} value={w.older_than_days ?? ''} onChange={(e) => setWhen({ older_than_days: num(e.target.value) })} /></Field>
            <div className="grid grid-cols-2 gap-2">
              <Field label="Bigger than (MB)"><Input type="number" min={0} value={w.larger_than_mb ?? ''} onChange={(e) => setWhen({ larger_than_mb: num(e.target.value) })} /></Field>
              <Field label="Smaller than (MB)"><Input type="number" min={0} value={w.smaller_than_mb ?? ''} onChange={(e) => setWhen({ smaller_than_mb: num(e.target.value) })} /></Field>
            </div>
          </div>
        </fieldset>

        <fieldset className="space-y-3 rounded-md border border-line p-3">
          <legend className="px-1 text-[12px] font-semibold text-muted">Then</legend>
          <select className={SELECT} value={a.type} onChange={(e) => {
            const t = e.target.value as RuleAction['type']
            setThen(t === 'tag' ? { type: 'tag', tags: [] } : t === 'move' ? { type: 'move', folder: '' } : t === 'copy' ? { type: 'copy', device_id: null, root_id: '', folder: '' } : { type: 'trash' })
          }}>
            <option value="move">Move them to a folder</option>
            <option value="copy">Copy them to another device or storage</option>
            <option value="tag">Tag them</option>
            <option value="trash">Move them to the trash</option>
          </select>
          {a.type === 'move' && (
            <Field label="Folder" hint="Can use {year}, {month}, {kind} and {ext}: Photos/{year} puts each photo in the folder for the year it was taken.">
              <Input value={a.folder} onChange={(e) => setThen({ type: 'move', folder: e.target.value })} placeholder="Documents/Invoices" />
            </Field>
          )}
          {a.type === 'tag' && (
            <Field label="Tags" hint="Separate tags with commas.">
              <Input value={a.tags.join(', ')} onChange={(e) => setThen({ type: 'tag', tags: e.target.value.split(',').map((t) => t.trimStart()) })} placeholder="tax 2026" />
            </Field>
          )}
          {a.type === 'copy' && (
            <div className="grid gap-3 sm:grid-cols-3">
              <Field label="Device">
                <select className={SELECT} value={copyDevice} onChange={(e) => setThen({ ...a, device_id: e.target.value || null, root_id: '' })}>
                  <option value="">This computer</option>
                  {peers.map((p) => <option key={p.id} value={p.id}>{p.name}{p.status !== 'online' ? ' (offline)' : ''}</option>)}
                </select>
              </Field>
              <Field label="Storage">
                {copyRoots === null ? <Spinner /> : (
                  <select className={SELECT} value={a.root_id} onChange={(e) => setThen({ ...a, root_id: e.target.value })}>
                    {copyRoots.length === 0 && <option value="">None available</option>}
                    {copyRoots.map((r) => <option key={r.id} value={r.id}>{r.name}</option>)}
                  </select>
                )}
              </Field>
              <Field label="Folder (must exist)"><Input value={a.folder} onChange={(e) => setThen({ ...a, folder: e.target.value })} placeholder="Top folder" /></Field>
            </div>
          )}
          {a.type === 'trash' && <p className="text-[12.5px] text-muted">Files can be restored from the trash until it is emptied.</p>}
        </fieldset>

        {d.trigger === 'added' && (
          <p className="text-[12px] text-subtle">This rule deals with files that arrive from now on. To include the files already there, use Run now after saving.</p>
        )}
        {error && <ErrorNote>{error}</ErrorNote>}
      </div>
      {preview && <PreviewModal title="What this rule would do now" load={() => rulesApi.previewDraft(draft)} onClose={() => setPreview(false)} />}
    </Modal>
  )
}
