import { useEffect, useState } from 'react'
import { FileText, ScanLine, Search } from 'lucide-react'
import {
  androidBridge, api, docApi, isMobile, peerPrefix, thisDevice,
  type DocDetails, type DocItem, type FileEntry,
} from '../services/api'
import { useResource } from '../hooks/useResource'
import { navigate, type Route } from '../hooks/useRoute'
import { FilePreview } from '../components/FilePreview'
import { toast } from '../components/Toasts'
import { scanTarget, setScanTarget, type ScanTarget } from '../lib/scans'
import { Badge, Button, EmptyState, ErrorNote, Field, Input, Modal, PageHeader, Spinner, cx } from '../components/ui'

const PAGE = 100
const SELECT = 'h-8 rounded-md border border-line bg-panel px-2 text-[13px] text-fg'

/** `2026-03-05` as the reader writes dates. */
function dayLabel(day: string | null) {
  if (!day) return ''
  const [y, m, d] = day.split('-').map(Number)
  return new Date(Date.UTC(y, m - 1, d)).toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric', timeZone: 'UTC' })
}

const stem = (name: string) => name.replace(/\.[^.]+$/, '')
const tagsOf = (d: { tags: string | null }) => (d.tags ? d.tags.split('|') : [])

export function Documents({ route }: { route: Route }) {
  const deviceId = route.params.get('device') ?? (isMobile() ? null : '')
  const devices = useResource(() => api.devices().catch(() => []), [])
  const peers = (devices.data ?? []).filter((d) => !d.is_local && d.online)
  const prefix = deviceId ? peerPrefix(deviceId) : ''
  const dapi = docApi(prefix)

  const [typed, setTyped] = useState(route.params.get('q') ?? '')
  const [query, setQuery] = useState(typed)
  const tag = route.params.get('tag') ?? ''
  const year = route.params.get('year') ?? ''
  const sender = route.params.get('sender') ?? ''
  const [limit, setLimit] = useState(PAGE)
  const [open, setOpen] = useState<DocItem | null>(null)
  const [preview, setPreview] = useState<FileEntry | null>(null)

  useEffect(() => {
    const t = setTimeout(() => setQuery(typed.trim()), 300)
    return () => clearTimeout(t)
  }, [typed])
  useEffect(() => setLimit(PAGE), [prefix, query, tag, year, sender])

  const docs = useResource(
    () => (deviceId === null ? Promise.resolve(null) : dapi.list({ q: query || undefined, tag: tag || undefined, year: year || undefined, sender: sender || undefined, limit })),
    [prefix, deviceId, query, tag, year, sender, limit],
  )

  const go = (p: { device?: string | null; tag?: string; year?: string; sender?: string }) =>
    navigate('documents', {
      device: (p.device !== undefined ? p.device : deviceId) || undefined,
      tag: (p.tag ?? tag) || undefined,
      year: (p.year ?? year) || undefined,
      sender: (p.sender ?? sender) || undefined,
    })

  const deviceName = (devices.data ?? []).find((d) => d.id === deviceId)?.name
  const canScan = isMobile() && !!androidBridge()?.scanDocument
  const filtered = !!(query || tag || year || sender)
  const data = docs.data

  return (
    <div>
      <PageHeader
        title="Documents"
        subtitle={deviceId ? `On ${deviceName ?? 'a paired device'}` : `On this ${thisDevice()}`}
        actions={
          <>
            {(peers.length > 0 || isMobile()) && (
              <select className={SELECT} value={deviceId ?? ''} onChange={(e) => go({ device: e.target.value, tag: '', year: '', sender: '' })}>
                {!isMobile() && <option value="">This {thisDevice()}</option>}
                {isMobile() && deviceId === null && <option value="">Choose a computer</option>}
                {peers.map((d) => <option key={d.id} value={d.id}>{d.name}</option>)}
              </select>
            )}
            {canScan && (
              <Button variant="primary" icon={<ScanLine className="size-4" />} onClick={() => androidBridge()?.scanDocument?.()}>
                Scan
              </Button>
            )}
          </>
        }
      />

      {canScan && <ScanDestination />}

      {deviceId === null ? (
        <EmptyState icon={<FileText className="size-6" />} title="Choose a computer">
          Pick a paired computer above to see its documents{canScan ? ', or scan a paper document with Scan.' : '.'}
        </EmptyState>
      ) : (
        <>
          <div className="mb-4 flex flex-wrap items-center gap-2">
            <div className="relative min-w-[200px] flex-1">
              <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-subtle" />
              <Input
                value={typed}
                onChange={(e) => setTyped(e.target.value)}
                placeholder="Search the text of documents"
                className="pl-8"
                type="search"
              />
            </div>
            <select className={SELECT} value={tag} onChange={(e) => go({ tag: e.target.value })} aria-label="Type">
              <option value="">All types</option>
              {(data?.tags ?? []).map((t) => <option key={t.value} value={t.value}>{t.value} ({t.count})</option>)}
            </select>
            <select className={SELECT} value={year} onChange={(e) => go({ year: e.target.value })} aria-label="Year">
              <option value="">Any year</option>
              {(data?.years ?? []).map((y) => <option key={y.value} value={y.value}>{y.value} ({y.count})</option>)}
            </select>
            <select className={cx(SELECT, 'max-w-[11rem]')} value={sender} onChange={(e) => go({ sender: e.target.value })} aria-label="From">
              <option value="">Anyone</option>
              {(data?.senders ?? []).map((s) => <option key={s.value} value={s.value}>{s.value} ({s.count})</option>)}
            </select>
          </div>

          {docs.error ? (
            <ErrorNote>{docs.error.message}</ErrorNote>
          ) : !data ? (
            <Spinner />
          ) : data.items.length === 0 ? (
            filtered ? (
              <EmptyState icon={<Search className="size-6" />} title="Nothing matches">
                No document says that or fits these filters.
              </EmptyState>
            ) : (
              <EmptyState icon={<FileText className="size-6" />} title="No documents yet">
                PDFs, Office files and photos of receipts or invoices show up here once HearthDrive has read them. That runs in the background after you add a folder.
              </EmptyState>
            )
          ) : (
            <>
              <ul className="divide-y divide-line overflow-hidden rounded-lg border border-line bg-panel">
                {data.items.map((d) => (
                  <li key={d.id}>
                    <DocRow doc={d} thumb={dapi.url(d.thumb_url)} onOpen={() => setOpen(d)} />
                  </li>
                ))}
              </ul>
              {data.more && (
                <div className="mt-3 flex justify-center">
                  <Button onClick={() => setLimit(limit + PAGE)}>Show more</Button>
                </div>
              )}
            </>
          )}
        </>
      )}

      {open && (
        <DocDetailsDialog
          doc={open}
          prefix={prefix}
          thumb={dapi.url(open.thumb_url)}
          onClose={() => setOpen(null)}
          onSaved={() => docs.reload()}
          onPreview={() => setPreview(open)}
        />
      )}
      <FilePreview
        file={preview}
        siblings={preview ? [preview] : []}
        onNavigate={setPreview}
        onClose={() => setPreview(null)}
        prefix={prefix}
      />
    </div>
  )
}

function DocRow({ doc, thumb, onOpen }: { doc: DocItem; thumb: string | null; onOpen: () => void }) {
  const tags = tagsOf(doc)
  return (
    <button onClick={onOpen} className="flex w-full items-center gap-3 px-3 py-2.5 text-left hover:bg-hover active:bg-hover">
      <span className="flex h-14 w-11 shrink-0 items-center justify-center overflow-hidden rounded-sm border border-line bg-white">
        {thumb ? (
          <img src={thumb} alt="" loading="lazy" decoding="async" className="size-full object-cover object-top" />
        ) : (
          <FileText className="size-5 text-neutral-400" />
        )}
      </span>
      <span className="min-w-0 flex-1">
        <span className="block truncate font-medium">{doc.title || stem(doc.filename)}</span>
        <span className="block truncate text-[12.5px] text-muted">
          {[doc.sender, dayLabel(doc.doc_date)].filter(Boolean).join(' · ')}
        </span>
        {tags.length > 0 && (
          <span className="mt-1 flex flex-wrap gap-1">
            {tags.slice(0, 3).map((t) => <Badge key={t}>{t}</Badge>)}
          </span>
        )}
      </span>
      {doc.amount && <span className="shrink-0 text-[13px] font-medium tabular">{doc.amount}</span>}
    </button>
  )
}

const READ_BY: Record<string, string> = {
  pdf: 'Text read from the PDF',
  office: 'Text read from the document',
  text: 'Plain text',
  ocr: 'Text read with Windows text recognition',
}

function DocDetailsDialog({ doc, prefix, thumb, onClose, onSaved, onPreview }: {
  doc: DocItem
  prefix: string
  thumb: string | null
  onClose: () => void
  onSaved: () => void
  onPreview: () => void
}) {
  const dapi = docApi(prefix)
  const [details, setDetails] = useState<DocDetails | null>(null)
  const [form, setForm] = useState({ title: '', date: '', sender: '', amount: '', note: '' })
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  useEffect(() => {
    let live = true
    dapi.get(doc.id).then((d) => {
      if (!live) return
      setDetails(d)
      setForm({ title: d.meta.title ?? '', date: d.meta.doc_date ?? '', sender: d.meta.sender ?? '', amount: d.meta.amount ?? '', note: d.meta.note ?? '' })
    }, (e) => live && setError((e as Error).message))
    return () => { live = false }
  }, [doc.id, prefix])

  const save = async () => {
    setBusy(true)
    setError('')
    try {
      setDetails(await dapi.update(doc.id, form))
      toast('Saved')
      onSaved()
      onClose()
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  const m = details?.meta
  const edit = details?.can_edit ?? false
  const set = (k: keyof typeof form) => (e: { target: { value: string } }) => setForm({ ...form, [k]: e.target.value })
  const found = (value: string | null | undefined, show = (v: string) => v) =>
    value ? <>Found in the text: {show(value)}</> : undefined

  return (
    <Modal
      open
      wide
      onClose={onClose}
      title={doc.title || stem(doc.filename)}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>{edit ? 'Cancel' : 'Close'}</Button>
          {edit && <Button variant="primary" loading={busy} disabled={!details} onClick={() => void save()}>Save</Button>}
        </>
      }
    >
      {!details && !error ? (
        <Spinner />
      ) : (
        <div className="flex flex-col gap-5 sm:flex-row">
          <div className="flex shrink-0 flex-col items-center gap-2 sm:w-40">
            <button
              onClick={onPreview}
              className="flex h-52 w-40 items-center justify-center overflow-hidden rounded border border-line bg-white"
              title="Open"
            >
              {thumb ? <img src={thumb} alt="" className="size-full object-contain object-top" /> : <FileText className="size-8 text-neutral-400" />}
            </button>
            <Button size="sm" onClick={onPreview}>Open</Button>
            <p className="max-w-40 truncate text-center text-[12px] text-subtle" title={doc.relative_path}>{doc.filename}</p>
          </div>
          <div className="min-w-0 flex-1 space-y-3">
            <Field label="Title">
              <Input value={form.title} onChange={set('title')} placeholder={stem(doc.filename)} disabled={!edit} maxLength={200} />
            </Field>
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <Field label="Date" hint={found(m?.found_date, dayLabel)}>
                <Input type="date" value={form.date} onChange={set('date')} disabled={!edit} />
              </Field>
              <Field label="Amount" hint={found(m?.found_amount)}>
                <Input value={form.amount} onChange={set('amount')} placeholder={m?.found_amount ?? ''} disabled={!edit} maxLength={40} />
              </Field>
            </div>
            <Field label="From" hint={found(m?.found_sender)}>
              <Input value={form.sender} onChange={set('sender')} placeholder={m?.found_sender ?? ''} disabled={!edit} maxLength={120} />
            </Field>
            <Field label="Note">
              <textarea
                value={form.note}
                onChange={set('note')}
                disabled={!edit}
                rows={3}
                maxLength={4000}
                className="w-full rounded-md border border-line bg-panel px-2.5 py-1.5 text-[13px] text-fg placeholder:text-subtle focus:border-accent/70 focus:ring-2 focus:ring-accent-soft focus:outline-none"
              />
            </Field>
            {details && details.tags.length > 0 && (
              <div className="flex flex-wrap gap-1">
                {details.tags.map((t) => <Badge key={t.tag} tone={t.auto ? 'neutral' : 'accent'}>{t.tag}</Badge>)}
              </div>
            )}
            <p className="text-[12px] text-subtle">
              {details?.text?.error
                ? `The text could not be read: ${details.text.error}`
                : details?.text?.chars
                  ? READ_BY[details.text.method ?? ''] ?? 'Text read'
                  : 'No text read yet'}
              {' · '}Leave a field empty to use what was found in the text.
            </p>
            <ErrorNote>{error}</ErrorNote>
          </div>
        </div>
      )}
    </Modal>
  )
}

/** Where scans from this phone go, with a way to choose again. */
function ScanDestination() {
  const [target, setTarget] = useState<ScanTarget | null>(scanTarget)
  useEffect(() => {
    const on = () => setTarget(scanTarget())
    window.addEventListener('nexus-scan-target', on)
    return () => window.removeEventListener('nexus-scan-target', on)
  }, [])
  return (
    <p className="mb-3 text-[12.5px] text-muted">
      {target ? (
        <>
          Scans go to {target.device_name}
          {target.path ? ` › ${target.path}` : ''}.{' '}
          <button className="text-accent underline-offset-2 hover:underline" onClick={() => setScanTarget(null)}>
            Choose again
          </button>
        </>
      ) : (
        'Scan a paper document: the first scan asks which computer and folder to keep scans in.'
      )}
    </p>
  )
}
