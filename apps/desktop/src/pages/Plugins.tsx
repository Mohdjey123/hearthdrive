import { useMemo, useState } from 'react'
import { CalendarClock, FileSpreadsheet, FileText, FolderCode, ImageDown, LayoutGrid, MapPinOff, Puzzle, ShieldCheck } from 'lucide-react'
import {
  fileApi, toolsApi,
  type PluginInfo, type PluginSetting, type PluginSettingValue, type StorageRoot, type StorePlugin,
} from '../services/api'
import { useResource } from '../hooks/useResource'
import { useSession } from '../stores/session'
import { toast, toastError } from '../components/Toasts'
import { Badge, Button, EmptyState, ErrorNote, Field, Input, Modal, PageHeader, Panel, Spinner, cx } from '../components/ui'

const SELECT = 'h-8 w-full rounded-md border border-line bg-panel px-2 text-[13px]'

const ICONS: Record<string, typeof Puzzle> = {
  'web-copies': ImageDown,
  'no-location': MapPinOff,
  'contact-sheet': LayoutGrid,
  'rename-by-date': CalendarClock,
  'document-text': FileText,
  checksums: ShieldCheck,
  'folder-list': FileSpreadsheet,
}

type Filter = 'all' | 'installed' | string

function status(p: StorePlugin): { label: string; tone: 'accent' | 'warn' | 'neutral' } | null {
  if (!p.installed) return null
  if (p.installed.needs_more) return { label: 'Needs your OK', tone: 'warn' }
  return p.installed.enabled ? { label: 'On', tone: 'accent' } : { label: 'Off', tone: 'neutral' }
}

function where(p: StorePlugin, roots: StorageRoot[]) {
  const ids = p.installed?.roots
  if (!ids) return 'All storage locations'
  const names = ids.map((id) => roots.find((r) => r.id === id)?.name ?? 'A removed location')
  return names.join(', ')
}

function usedOn(c: StorePlugin['commands'][number]) {
  if (c.for === 'folder') return 'folders'
  if (c.extensions.length === 1) return `.${c.extensions[0]} files`
  if (c.extensions.some((e) => e === 'jpg') && c.extensions.length > 3) return 'photos'
  if (c.extensions.every((e) => ['jpg', 'jpeg', 'jfif'].includes(e))) return 'JPEG photos'
  return 'documents'
}

/** The plugin store: plugins that come with HearthDrive, plus any in the plugins folder. */
export function Plugins() {
  const { phase } = useSession()
  const me = phase.kind === 'ready' ? phase.me : null
  const owner = me?.role === 'owner'
  const allowed = me?.role === 'owner' || me?.role === 'administrator'
  const list = useResource(() => (allowed ? toolsApi.plugins() : Promise.resolve(null)), [allowed])
  const roots = useResource(() => fileApi().roots(), [])
  const [filter, setFilter] = useState<Filter>('all')
  const [open, setOpen] = useState<string | null>(null)

  const store = list.data?.store ?? []
  const categories = useMemo(() => [...new Set(store.map((p) => p.category))], [store])
  const shown = store.filter((p) => filter === 'all' || (filter === 'installed' ? !!p.installed : p.category === filter))
  const installedCount = store.filter((p) => p.installed).length
  const current = store.find((p) => p.id === open) ?? null

  if (!allowed) {
    return (
      <>
        <PageHeader title="Plugins" />
        <EmptyState icon={<Puzzle className="size-8" />} title="Plugins are set up by the owner">
          The owner of this computer chooses which plugins it has. Ask them if you need one.
        </EmptyState>
      </>
    )
  }

  return (
    <>
      <PageHeader
        title="Plugins"
        subtitle="Extra commands for the puzzle menu in Files"
      />
      <p className="-mt-3 mb-4 text-[12.5px] text-subtle">They run on this computer, only where and how you allow.</p>
      {list.error && <ErrorNote>{list.error.message}</ErrorNote>}
      {!list.data && !list.error && <Spinner />}
      {list.data && (
        <>
          <div className="mb-4 flex flex-wrap gap-1.5" role="tablist" aria-label="Show">
            {(['all', 'installed', ...categories] as Filter[]).map((f) => (
              <button
                key={f}
                role="tab"
                aria-selected={filter === f}
                onClick={() => setFilter(f)}
                className={cx(
                  'h-7 rounded-full border px-3 text-[12.5px] transition-colors',
                  filter === f ? 'border-accent bg-accent-soft font-medium text-fg' : 'border-line text-muted hover:bg-hover hover:text-fg',
                )}
              >
                {f === 'all' ? 'All' : f === 'installed' ? `Installed (${installedCount})` : f}
              </button>
            ))}
          </div>

          {shown.length === 0 ? (
            <p className="mb-6 text-muted">No plugins installed yet. Pick one under All.</p>
          ) : (
            <ul className="mb-6 grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3">
              {shown.map((p) => (
                <li key={p.id}>
                  <PluginCard plugin={p} roots={roots.data ?? []} onOpen={() => setOpen(p.id)} />
                </li>
              ))}
            </ul>
          )}

          <OwnPlugins folder={list.data.folder} plugins={list.data.plugins} owner={owner} reload={list.reload} />
        </>
      )}

      {current && (
        <PluginDialog
          key={`${current.id}/${current.installed?.installed_at ?? 'new'}`}
          plugin={current}
          roots={roots.data ?? []}
          owner={owner}
          onClose={() => setOpen(null)}
          onChanged={list.reload}
        />
      )}
    </>
  )
}

function PluginCard({ plugin: p, roots, onOpen }: { plugin: StorePlugin; roots: StorageRoot[]; onOpen: () => void }) {
  const Icon = ICONS[p.id] ?? Puzzle
  const s = status(p)
  return (
    <button
      onClick={onOpen}
      className="flex h-full w-full gap-3 rounded-lg border border-line bg-panel p-4 text-left transition-colors hover:border-line-strong hover:bg-hover"
    >
      <span className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-accent-soft text-accent">
        <Icon className="size-5" />
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <span className="font-medium">{p.name}</span>
          {s && <Badge tone={s.tone}>{s.label}</Badge>}
        </span>
        <span className="mt-0.5 block text-muted">{p.summary}</span>
        <span className="mt-2 block text-[12px] text-subtle">
          {p.category} · Needs to {needsShort(p)}
          {p.installed && <> · {where(p, roots)}</>}
        </span>
      </span>
    </button>
  )
}

function needsShort(p: StorePlugin) {
  const words = p.needs.map((n) => (n === 'read' ? 'read files' : n === 'add' ? 'add files' : 'rename files'))
  return words.length > 1 ? `${words.slice(0, -1).join(', ')} and ${words[words.length - 1]}` : words[0]
}

/** Where a plugin may work: everywhere, or only some storage locations. */
function RootsPicker({ roots, value, onChange, disabled }: {
  roots: StorageRoot[]
  value: string[] | null
  onChange: (v: string[] | null) => void
  disabled?: boolean
}) {
  const everywhere = value === null
  return (
    <div className="space-y-2">
      <label className="flex items-center gap-2">
        <input type="radio" className="accent-[var(--nx-accent)]" checked={everywhere} disabled={disabled} onChange={() => onChange(null)} />
        All storage locations, including ones added later
      </label>
      <label className="flex items-center gap-2">
        <input
          type="radio"
          className="accent-[var(--nx-accent)]"
          checked={!everywhere}
          disabled={disabled || roots.length === 0}
          onChange={() => onChange(roots.slice(0, 1).map((r) => r.id))}
        />
        Only these:
      </label>
      {!everywhere && (
        <ul className="ml-6 divide-y divide-line rounded-md border border-line">
          {roots.map((r) => (
            <li key={r.id}>
              <label className="flex items-center gap-2.5 px-3 py-2">
                <input
                  type="checkbox"
                  className="accent-[var(--nx-accent)]"
                  disabled={disabled}
                  checked={value.includes(r.id)}
                  onChange={(e) => onChange(e.target.checked ? [...value, r.id] : value.filter((x) => x !== r.id))}
                />
                <span className="min-w-0 flex-1 truncate">{r.name}</span>
                {r.read_only && <span className="text-[12px] text-subtle">read-only</span>}
              </label>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

function SettingInput({ setting: s, value, onChange, disabled }: {
  setting: PluginSetting
  value: PluginSettingValue
  onChange: (v: PluginSettingValue) => void
  disabled?: boolean
}) {
  if (s.type === 'toggle') {
    return (
      <label className="flex items-start gap-2.5">
        <input type="checkbox" className="mt-0.5 accent-[var(--nx-accent)]" checked={!!value} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
        <span>
          {s.label}
          {s.help && <span className="block text-[12px] text-subtle">{s.help}</span>}
        </span>
      </label>
    )
  }
  return (
    <Field label={s.label} hint={s.help || (s.type === 'number' ? `${s.min} to ${s.max}${s.unit ? ` ${s.unit}` : ''}` : undefined)}>
      {s.type === 'choice' ? (
        <select className={SELECT} value={String(value)} disabled={disabled} onChange={(e) => onChange(e.target.value)}>
          {s.options.map((o) => <option key={o.value} value={o.value}>{o.label}</option>)}
        </select>
      ) : (
        <Input type="number" min={s.min} max={s.max} value={String(value)} disabled={disabled} className="max-w-32"
          onChange={(e) => onChange(e.target.value === '' ? '' : Number(e.target.value))} />
      )}
    </Field>
  )
}

function PluginDialog({ plugin: p, roots, owner, onClose, onChanged }: {
  plugin: StorePlugin
  roots: StorageRoot[]
  owner: boolean
  onClose: () => void
  onChanged: () => void
}) {
  const inst = p.installed
  const Icon = ICONS[p.id] ?? Puzzle
  const [scope, setScope] = useState<string[] | null>(inst ? inst.roots : null)
  const [values, setValues] = useState<Record<string, PluginSettingValue>>(inst?.settings ?? {})
  const [busy, setBusy] = useState<string | null>(null)
  const scopeChanged = !!inst && JSON.stringify(scope) !== JSON.stringify(inst.roots)
  const settingsChanged = !!inst && p.settings.some((s) => values[s.key] !== inst.settings[s.key])
  const invalidScope = scope !== null && scope.length === 0

  const act = async (what: string, fn: () => Promise<unknown>, done?: string) => {
    setBusy(what)
    try {
      await fn()
      if (done) toast(done)
      onChanged()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(null)
    }
  }

  const save = () => {
    const change: { roots?: string[] | null; settings?: Record<string, PluginSettingValue> } = {}
    if (scopeChanged) change.roots = scope
    if (settingsChanged) change.settings = Object.fromEntries(p.settings.map((s) => [s.key, values[s.key]]))
    return act('save', () => toolsApi.changePlugin(p.id, change), `${p.name}: saved`)
  }

  const remove = () => {
    if (!confirm(`Remove ${p.name}? Its settings go too. Files it made stay where they are.`)) return
    void act('remove', () => toolsApi.removePlugin(p.id), `${p.name} removed`).then(onClose)
  }

  return (
    <Modal
      open
      title={p.name}
      onClose={onClose}
      footer={
        !owner ? <Button onClick={onClose}>Close</Button> : !inst ? (
          <>
            <Button variant="ghost" onClick={onClose}>Cancel</Button>
            <Button variant="primary" loading={busy === 'install'} disabled={invalidScope}
              onClick={() => void act('install', () => toolsApi.installPlugin(p.id, scope), `${p.name} installed`)}>
              Install
            </Button>
          </>
        ) : (
          <>
            <Button variant="danger" className="mr-auto" loading={busy === 'remove'} onClick={remove}>Remove</Button>
            {inst.needs_more ? (
              <Button variant="primary" loading={busy === 'toggle'} onClick={() => void act('toggle', () => toolsApi.changePlugin(p.id, { enabled: true }), `${p.name} is on`)}>
                Allow and turn on
              </Button>
            ) : (
              <Button loading={busy === 'toggle'}
                onClick={() => void act('toggle', () => toolsApi.changePlugin(p.id, { enabled: !inst.enabled }), `${p.name} is ${inst.enabled ? 'off' : 'on'}`)}>
                {inst.enabled ? 'Turn off' : 'Turn on'}
              </Button>
            )}
            <Button variant="primary" loading={busy === 'save'} disabled={(!scopeChanged && !settingsChanged) || invalidScope} onClick={() => void save()}>
              Save
            </Button>
          </>
        )
      }
    >
      <div className="space-y-5 text-[13px]">
        <div className="flex gap-3">
          <span className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-accent-soft text-accent">
            <Icon className="size-5" />
          </span>
          <div className="min-w-0">
            <p>{p.description}</p>
            <p className="mt-1 text-[12px] text-subtle">{p.category} · version {p.version} · comes with HearthDrive</p>
          </div>
        </div>

        {inst?.needs_more && (
          <p className="rounded-md border border-warn/30 bg-warn-soft px-3 py-2 text-warn">
            This version needs more than you allowed before, so it is off. Check the list below, then allow it or remove it.
          </p>
        )}

        <section>
          <h3 className="mb-1.5 text-[12px] font-semibold tracking-wide text-muted uppercase">It needs to</h3>
          <ul className="space-y-1">
            {p.needs_text.map((n) => <li key={n} className="flex gap-2"><span className="text-accent">•</span>{n}</li>)}
          </ul>
          <p className="mt-1.5 text-[12px] text-subtle">
            Nothing else: it cannot reach the internet, other devices or files outside the storage locations below.
          </p>
        </section>

        <section>
          <h3 className="mb-1.5 text-[12px] font-semibold tracking-wide text-muted uppercase">In the puzzle menu</h3>
          <ul className="space-y-1">
            {p.commands.map((c) => (
              <li key={c.id}>
                {c.label} <span className="text-subtle">on {usedOn(c)}</span>
              </li>
            ))}
          </ul>
        </section>

        <section>
          <h3 className="mb-1.5 text-[12px] font-semibold tracking-wide text-muted uppercase">Where it may work</h3>
          {owner ? (
            <RootsPicker roots={roots} value={scope} onChange={setScope} />
          ) : (
            <p>{inst ? where(p, roots) : 'Not installed'}</p>
          )}
          {invalidScope && <p className="mt-1 text-[12px] text-danger">Pick at least one.</p>}
        </section>

        {inst && p.settings.length > 0 && (
          <section className="space-y-3">
            <h3 className="text-[12px] font-semibold tracking-wide text-muted uppercase">Settings</h3>
            {p.settings.map((s) => (
              <SettingInput key={s.key} setting={s} value={values[s.key] ?? s.default} disabled={!owner}
                onChange={(v) => setValues({ ...values, [s.key]: v })} />
            ))}
          </section>
        )}
        {!inst && p.settings.length > 0 && <p className="text-[12px] text-subtle">It has settings you can change once it is installed.</p>}
        {!owner && <p className="text-[12px] text-subtle">Only the owner of this computer can install plugins or change them.</p>}
      </div>
    </Modal>
  )
}

/** Plugins someone made: a folder with a plugin.json that runs a program. */
function OwnPlugins({ folder, plugins, owner, reload }: { folder: string; plugins: PluginInfo[]; owner: boolean; reload: () => void }) {
  const [busy, setBusy] = useState<string | null>(null)

  const toggle = async (p: PluginInfo) => {
    if (!p.enabled && !confirm(`Turn on “${p.manifest?.name ?? p.id}”? Its commands run programs on this computer with your rights, and can do anything you can.`)) return
    setBusy(p.id)
    try {
      await toolsApi.setPlugin(p.id, !p.enabled)
      reload()
    } catch (e) {
      toastError(e)
    } finally {
      setBusy(null)
    }
  }

  return (
    <Panel title="Your own plugins" actions={<Button variant="ghost" size="sm" onClick={reload}>Look again</Button>}>
      <div className="space-y-3 text-[13px]">
        <p className="text-muted">
          A plugin can also be a folder with a <code>plugin.json</code> that runs a program on this computer. Unlike the plugins above,
          such a program can do anything you can, so only turn on ones you trust. Put them in{' '}
          <code className="break-all">{folder}</code>.
        </p>
        {plugins.length === 0 && <p className="flex items-center gap-2 text-subtle"><FolderCode className="size-4" /> None yet.</p>}
        {plugins.map((p) => (
          <div key={p.id} className="rounded-md border border-line p-3">
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-2 font-medium">
                  {p.manifest?.name ?? p.id}
                  {p.manifest?.version && <span className="font-normal text-subtle">{p.manifest.version}</span>}
                  {p.enabled ? <Badge tone="accent">On</Badge> : <Badge>Off</Badge>}
                </div>
                {p.manifest?.description && <p className="text-muted">{p.manifest.description}</p>}
              </div>
              {owner && p.manifest && (
                <Button loading={busy === p.id} variant={p.enabled ? 'ghost' : 'secondary'} onClick={() => void toggle(p)}>
                  {p.enabled ? 'Turn off' : 'Turn on'}
                </Button>
              )}
            </div>
            {p.error && <div className="mt-2"><ErrorNote>{p.error}</ErrorNote></div>}
            {p.changed && <p className="mt-1 text-warn">Its plugin.json changed since it was turned on, so it is off until you turn it on again.</p>}
            {p.manifest && p.manifest.commands.length > 0 && (
              <ul className="mt-2 space-y-1">
                {p.manifest.commands.map((c) => (
                  <li key={c.id}>
                    {c.label} <span className="text-subtle">({c.for === 'none' ? 'anywhere' : c.for}{c.extensions.length ? `: ${c.extensions.join(', ')}` : ''})</span>
                    <code className="block truncate text-[11.5px] text-subtle">{[c.program, ...(c.args ?? [])].join(' ')}</code>
                  </li>
                ))}
              </ul>
            )}
          </div>
        ))}
      </div>
    </Panel>
  )
}
