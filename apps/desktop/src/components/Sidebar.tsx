import { useEffect, useRef, useState } from 'react'
import { ArrowUpDown, Clapperboard, Copy, Folder, Images, FolderSync, HardDrive, HeartPulse, History, ImageUp, LayoutGrid, Link2, Menu, MonitorSmartphone, Settings, Wand2 } from 'lucide-react'
import type { Page } from '../hooks/useRoute'
import { navigate } from '../hooks/useRoute'
import { subscribeStatus } from '../services/events'
import { useTransfers } from '../stores/transfers'
import { useSession } from '../stores/session'
import { api, isMobile } from '../services/api'
import { useResource } from '../hooks/useResource'
import { cx, StatusDot } from './ui'
import { hasTitleBar } from './TitleBar'

const NAV: { page: Page; label: string; icon: typeof Folder }[] = [
  { page: 'overview', label: 'Overview', icon: LayoutGrid },
  { page: 'files', label: 'Files', icon: Folder },
  { page: 'photos', label: 'Photos', icon: Images },
  { page: 'media', label: 'Media', icon: Clapperboard },
  { page: 'storage', label: 'Storage', icon: HardDrive },
  { page: 'automation', label: 'Automation', icon: Wand2 },
  { page: 'health', label: 'Drive health', icon: HeartPulse },
  { page: 'devices', label: 'Devices', icon: MonitorSmartphone },
  { page: 'sync', label: 'Sync', icon: FolderSync },
  { page: 'copies', label: 'Copies', icon: Copy },
  { page: 'backups', label: 'Backups', icon: History },
  { page: 'backup', label: 'Backup', icon: ImageUp },
  { page: 'shares', label: 'Sharing', icon: Link2 },
  { page: 'transfers', label: 'Transfers', icon: ArrowUpDown },
  { page: 'settings', label: 'Settings', icon: Settings },
]

/** Pages that need storage on this device. */
export const PHONELESS: Page[] = ['overview', 'storage', 'automation', 'health', 'sync', 'copies', 'backups']
/** Pages only phones have. */
export const PHONE_ONLY: Page[] = ['backup']

/** The pages this device shows. */
export function pageAvailable(page: Page) {
  return isMobile() ? !PHONELESS.includes(page) : !PHONE_ONLY.includes(page)
}

/** The HearthDrive mark: a hearth opening, drawn on a 32-unit grid. It takes
 *  the text colour; `micro` drops the opening for sizes under 20 px. */
export function Logo({ className, micro }: { className?: string; micro?: boolean }) {
  return (
    <svg viewBox="0 0 32 32" className={className} aria-hidden>
      <path
        d={micro ? 'M3.2 29V20.4A12.8 12.8 0 0 1 28.8 20.4V29Z' : 'M3.2 29V20.4A12.8 12.8 0 0 1 28.8 20.4V29ZM9.2 29V20.2A6.8 6.8 0 0 1 22.8 20.2V29Z'}
        fill="currentColor"
        fillRule="evenodd"
      />
    </svg>
  )
}

/** Uploads from this window plus copies and syncs running on the node. */
function useActiveTransfers() {
  const transfers = useTransfers()
  const jobs = useResource(() => api.jobs().catch(() => []), [], (e) => e.type === 'transfer.updated')
  const running = (jobs.data ?? []).filter((j) => j.status === 'queued' || j.status === 'running').length
  return transfers.filter((t) => t.status === 'uploading' || t.status === 'queued').length + running
}

export function Sidebar({ current }: { current: Page }) {
  const { phase } = useSession()
  const active = useActiveTransfers()
  const [live, setLive] = useState(false)
  useEffect(() => subscribeStatus(setLive), [])
  const nodeName = phase.kind === 'ready' ? phase.nodeName : ''
  const username = phase.kind === 'ready' ? phase.me.username : ''

  return (
    <aside className="hidden w-56 shrink-0 flex-col border-r border-line bg-panel md:flex">
      {/* The desktop app's title bar already shows the logo and name. */}
      {!hasTitleBar() && (
        <div className="flex h-14 items-center gap-2.5 px-4">
          <Logo className="size-6 text-fg" />
          <span className="text-[15px] font-bold">HearthDrive</span>
        </div>
      )}
      <nav className="flex-1 space-y-0.5 px-2 py-2">
        {NAV.filter((n) => pageAvailable(n.page)).map(({ page, label, icon: Icon }) => (
          <button
            key={page}
            onClick={() => navigate(page)}
            className={cx(
              'flex h-8 w-full items-center gap-2.5 rounded-md px-2.5 text-[13px] transition-colors',
              current === page ? 'bg-accent-soft text-fg font-medium' : 'text-muted hover:bg-hover hover:text-fg',
            )}
          >
            <Icon className={cx('size-4', current === page && 'text-accent')} />
            <span className="flex-1 text-left">{label}</span>
            {page === 'transfers' && active > 0 && (
              <span className="rounded bg-accent px-1.5 text-[11px] font-semibold text-accent-fg tabular">{active}</span>
            )}
          </button>
        ))}
      </nav>
      <div className="border-t border-line px-4 py-3">
        <div className="flex items-center gap-2">
          <StatusDot tone={live ? 'online' : 'offline'} />
          <span className="truncate text-[12.5px] font-medium">{nodeName}</span>
        </div>
        <div className="mt-0.5 truncate pl-4 text-[11.5px] text-subtle">
          {live ? 'Service connected' : 'Connecting…'} · {username}
        </div>
      </div>
    </aside>
  )
}

/** The bottom bar's own tabs; every other page this device has is under More. */
const TABS: Page[] = ['files', 'photos', 'devices']

/** Navigation for narrow screens: four tabs along the bottom edge, and a
 *  sheet with the rest behind More. Material's guidance is three to five
 *  destinations in a bottom bar; a phone has eight pages. */
export function BottomNav({ current }: { current: Page }) {
  const active = useActiveTransfers()
  const [open, setOpen] = useState(false)
  // A phone has no storage of its own, so nothing to show storage or sync for.
  const tabs = NAV.filter((n) => TABS.includes(n.page) && pageAvailable(n.page))
  const more = NAV.filter((n) => !TABS.includes(n.page) && pageAvailable(n.page))
  const inMore = more.some((n) => n.page === current)

  // The open sheet has its own history entry, so Android's Back button
  // (which steps back through the web view's history) closes it first.
  // Closing it any other way steps back over that entry, then runs `after`.
  const after = useRef<(() => void) | null>(null)
  const openSheet = () => {
    history.pushState({ moreSheet: true }, '')
    setOpen(true)
  }
  const closeSheet = (then?: () => void) => {
    if (history.state?.moreSheet) {
      after.current = then ?? null
      history.back()
    } else {
      setOpen(false)
      then?.()
    }
  }

  useEffect(() => {
    const onPop = () => {
      setOpen(false)
      const then = after.current
      after.current = null
      then?.()
    }
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  }, [])

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && closeSheet()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open])

  const go = (page: Page) => (open ? closeSheet(() => navigate(page)) : navigate(page))

  return (
    <>
      <nav aria-label="Main" className="flex shrink-0 border-t border-line bg-panel px-1 md:hidden">
        {tabs.map(({ page, label, icon }) => (
          <Tab key={page} label={label} icon={icon} selected={current === page && !open} onClick={() => go(page)} />
        ))}
        <Tab
          label="More"
          icon={Menu}
          selected={open || inMore}
          onClick={() => (open ? closeSheet() : openSheet())}
          expanded={open}
          dot={active > 0}
        />
      </nav>
      {open && (
        <div className="fixed inset-0 z-40 md:hidden" role="presentation">
          <button aria-label="Close More" className="absolute inset-0 bg-black/45" onClick={() => closeSheet()} />
          <div
            role="dialog"
            aria-label="More"
            className="sheet-up safe-bottom absolute inset-x-0 bottom-0 rounded-t-2xl border-t border-line bg-panel px-3 pt-2 pb-3"
          >
            <div className="mx-auto mb-2 h-1 w-9 rounded-full bg-line-strong" aria-hidden />
            <ul>
              {more.map(({ page, label, icon: Icon }) => (
                <li key={page}>
                  <button
                    onClick={() => go(page)}
                    className={cx(
                      'flex min-h-[52px] w-full items-center gap-4 rounded-xl px-3 text-left text-[15px]',
                      page === 'settings' && 'mt-1 border-t border-line rounded-t-none',
                      current === page ? 'bg-accent-soft font-medium text-fg' : 'text-fg active:bg-hover',
                    )}
                    aria-current={current === page ? 'page' : undefined}
                  >
                    <Icon className={cx('size-5 shrink-0', current === page ? 'text-accent' : 'text-muted')} />
                    <span className="flex-1">{label}</span>
                    {page === 'transfers' && active > 0 && (
                      <span className="text-[13px] text-muted tabular">{active} running</span>
                    )}
                  </button>
                </li>
              ))}
            </ul>
          </div>
        </div>
      )}
    </>
  )
}

function Tab({ label, icon: Icon, selected, onClick, expanded, dot }: {
  label: string
  icon: typeof Folder
  selected: boolean
  onClick: () => void
  expanded?: boolean
  dot?: boolean
}) {
  return (
    <button
      onClick={onClick}
      aria-current={selected && expanded === undefined ? 'page' : undefined}
      aria-expanded={expanded}
      className={cx(
        'flex min-h-[60px] min-w-0 flex-1 flex-col items-center justify-center gap-1 text-[12px]',
        selected ? 'font-semibold text-fg' : 'text-muted',
      )}
    >
      <span className={cx('relative flex h-7 w-14 items-center justify-center rounded-full transition-colors', selected && 'bg-accent-soft')}>
        <Icon className={cx('size-[22px]', selected && 'text-accent')} strokeWidth={selected ? 2.25 : 1.75} />
        {dot && <span className="absolute top-0.5 right-3 size-2 rounded-full bg-accent ring-2 ring-panel" aria-label="Transfers running" />}
      </span>
      <span className="max-w-full truncate">{label}</span>
    </button>
  )
}
