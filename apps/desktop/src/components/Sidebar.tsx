import { useEffect, useState } from 'react'
import { ArrowUpDown, Folder, FolderSync, HardDrive, ImageUp, LayoutGrid, Link2, MonitorSmartphone, Settings } from 'lucide-react'
import type { Page } from '../hooks/useRoute'
import { navigate } from '../hooks/useRoute'
import { subscribeStatus } from '../services/events'
import { useTransfers } from '../stores/transfers'
import { useSession } from '../stores/session'
import { api, isMobile } from '../services/api'
import { useResource } from '../hooks/useResource'
import { cx, StatusDot } from './ui'

const NAV: { page: Page; label: string; icon: typeof Folder }[] = [
  { page: 'overview', label: 'Overview', icon: LayoutGrid },
  { page: 'files', label: 'Files', icon: Folder },
  { page: 'storage', label: 'Storage', icon: HardDrive },
  { page: 'devices', label: 'Devices', icon: MonitorSmartphone },
  { page: 'sync', label: 'Sync', icon: FolderSync },
  { page: 'backup', label: 'Backup', icon: ImageUp },
  { page: 'shares', label: 'Shares', icon: Link2 },
  { page: 'transfers', label: 'Transfers', icon: ArrowUpDown },
  { page: 'settings', label: 'Settings', icon: Settings },
]

/** Pages that need storage on this device. */
export const PHONELESS: Page[] = ['overview', 'storage', 'sync']
/** Pages only phones have. */
export const PHONE_ONLY: Page[] = ['backup']

/** The pages this device shows. */
export function pageAvailable(page: Page) {
  return isMobile() ? !PHONELESS.includes(page) : !PHONE_ONLY.includes(page)
}

export function Logo({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 512 512" className={className} aria-hidden>
      <g stroke="var(--nx-accent)" strokeWidth="26" strokeLinecap="round">
        <line x1="150" y1="170" x2="362" y2="170" />
        <line x1="150" y1="170" x2="256" y2="356" />
        <line x1="362" y1="170" x2="256" y2="356" />
      </g>
      <circle cx="150" cy="170" r="62" fill="var(--nx-accent)" />
      <circle cx="362" cy="170" r="62" fill="var(--nx-accent)" />
      <circle cx="256" cy="356" r="62" fill="var(--nx-fg)" />
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
      <div className="flex h-14 items-center gap-2.5 px-4">
        <Logo className="size-6" />
        <span className="text-[14px] font-semibold tracking-tight">NEXUS Cloud</span>
      </div>
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

/** Navigation for narrow screens: a tab bar along the bottom edge. */
export function BottomNav({ current }: { current: Page }) {
  const active = useActiveTransfers()
  // A phone has no storage of its own, so nothing to show storage or sync for.
  const items = NAV.filter((n) => pageAvailable(n.page))
  return (
    <nav className="flex shrink-0 border-t border-line bg-panel md:hidden">
      {items.map(({ page, label, icon: Icon }) => (
        <button
          key={page}
          onClick={() => navigate(page)}
          className={cx(
            'relative flex min-w-0 flex-1 flex-col items-center gap-0.5 py-2 text-[10.5px]',
            current === page ? 'font-medium text-fg' : 'text-muted',
          )}
        >
          <Icon className={cx('size-5', current === page && 'text-accent')} />
          <span className="max-w-full truncate">{label}</span>
          {page === 'transfers' && active > 0 && (
            <span className="absolute top-1 left-1/2 ml-2 rounded bg-accent px-1 text-[10px] font-semibold text-accent-fg tabular">{active}</span>
          )}
        </button>
      ))}
    </nav>
  )
}
