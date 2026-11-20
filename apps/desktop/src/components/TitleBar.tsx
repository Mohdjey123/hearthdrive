import { useEffect, useState, type ReactNode } from 'react'
import { Copy, Minus, Square, X } from 'lucide-react'
import { isTauri } from '../services/api'
import { Logo } from './Sidebar'
import { cx } from './ui'

/** The desktop app draws its own title bar (the window has no frame), so
 *  the logo and name appear once and the bar matches the theme. Phones and
 *  plain browsers keep their own chrome. */
export const hasTitleBar = () =>
  isTauri() && !/Android|iPhone|iPad/i.test(typeof navigator === 'undefined' ? '' : navigator.userAgent)

async function appWindow() {
  const { getCurrentWindow } = await import('@tauri-apps/api/window')
  return getCurrentWindow()
}

/**
 * Logo, name and the window buttons. The empty space is a drag region:
 * dragging moves the window (with Windows snapping), double-clicking
 * maximises or restores it. Resizing works from the window's edges.
 */
export function TitleBar() {
  const [maximized, setMaximized] = useState(false)

  useEffect(() => {
    let unlisten: (() => void) | undefined
    let gone = false
    void appWindow().then(async (w) => {
      const update = () => void w.isMaximized().then((m) => !gone && setMaximized(m)).catch(() => {})
      update()
      const off = await w.onResized(update)
      if (gone) off()
      else unlisten = off
    })
    return () => {
      gone = true
      unlisten?.()
    }
  }, [])

  const act = (f: (w: Awaited<ReturnType<typeof appWindow>>) => Promise<void>) => () => void appWindow().then(f).catch(() => {})

  return (
    <header data-tauri-drag-region className="flex h-9 shrink-0 select-none items-center border-b border-line bg-panel">
      <div data-tauri-drag-region className="flex h-full flex-1 items-center gap-2 pl-4">
        <Logo className="pointer-events-none size-[18px] text-fg" />
        <span className="pointer-events-none text-[13px] font-semibold">HearthDrive</span>
      </div>
      <WindowButton label="Minimise" onClick={act((w) => w.minimize())}>
        <Minus className="size-4" strokeWidth={1.5} />
      </WindowButton>
      <WindowButton label={maximized ? 'Restore' : 'Maximise'} onClick={act((w) => w.toggleMaximize())}>
        {maximized ? <Copy className="size-3.5 -scale-x-100" strokeWidth={1.5} /> : <Square className="size-3.5" strokeWidth={1.5} />}
      </WindowButton>
      {/* Closing hides the window to the tray; Quit is in the tray's menu. */}
      <WindowButton label="Close" close onClick={act((w) => w.close())}>
        <X className="size-4" strokeWidth={1.5} />
      </WindowButton>
    </header>
  )
}

function WindowButton({ label, close, onClick, children }: { label: string; close?: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      tabIndex={-1}
      onClick={onClick}
      className={cx(
        'flex h-full w-[46px] items-center justify-center text-muted transition-colors',
        close ? 'hover:bg-[#c42b1c] hover:text-white active:bg-[#b22a1b]' : 'hover:bg-hover hover:text-fg',
      )}
    >
      {children}
    </button>
  )
}
