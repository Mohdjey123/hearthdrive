import { useEffect, useRef, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode } from 'react'
import { Loader2, X } from 'lucide-react'

const cx = (...c: (string | false | null | undefined)[]) => c.filter(Boolean).join(' ')
export { cx }

type Variant = 'primary' | 'secondary' | 'ghost' | 'danger'

export function Button({
  variant = 'secondary',
  size = 'md',
  icon,
  loading,
  className,
  children,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: 'sm' | 'md'; icon?: ReactNode; loading?: boolean }) {
  return (
    <button
      {...rest}
      disabled={rest.disabled || loading}
      className={cx(
        'inline-flex items-center justify-center gap-1.5 rounded-md font-medium whitespace-nowrap transition-colors select-none',
        'disabled:opacity-50 disabled:pointer-events-none',
        size === 'sm' ? 'h-7 px-2.5 text-[12px]' : 'h-8 px-3 text-[13px]',
        variant === 'primary' && 'bg-accent text-accent-fg hover:bg-accent-strong',
        variant === 'secondary' && 'bg-elevated text-fg border border-line hover:bg-hover hover:border-line-strong',
        variant === 'ghost' && 'text-muted hover:text-fg hover:bg-hover',
        variant === 'danger' && 'bg-danger-soft text-danger border border-transparent hover:border-danger/40',
        className,
      )}
    >
      {loading ? <Loader2 className="size-3.5 animate-spin" /> : icon}
      {children}
    </button>
  )
}

export function IconButton({ label, children, className, ...rest }: ButtonHTMLAttributes<HTMLButtonElement> & { label: string }) {
  return (
    <button
      {...rest}
      aria-label={label}
      title={label}
      className={cx(
        'inline-flex size-7 items-center justify-center rounded-md text-muted hover:text-fg hover:bg-hover transition-colors disabled:opacity-40 disabled:pointer-events-none',
        className,
      )}
    >
      {children}
    </button>
  )
}

export function Input({ className, ...rest }: InputHTMLAttributes<HTMLInputElement>) {
  return (
    <input
      {...rest}
      className={cx(
        'h-8 w-full rounded-md border border-line bg-panel px-2.5 text-[13px] text-fg placeholder:text-subtle',
        'focus:outline-none focus:border-accent/70 focus:ring-2 focus:ring-accent-soft',
        className,
      )}
    />
  )
}

export function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="block space-y-1.5">
      <span className="block text-[12px] font-medium text-muted">{label}</span>
      {children}
      {hint && <span className="block text-[12px] text-subtle">{hint}</span>}
    </label>
  )
}

export function Spinner({ className }: { className?: string }) {
  return <Loader2 className={cx('size-4 animate-spin text-muted', className)} />
}

export function Panel({ title, actions, children, className, padded = true }: {
  title?: ReactNode
  actions?: ReactNode
  children: ReactNode
  className?: string
  padded?: boolean
}) {
  return (
    <section className={cx('rounded-lg border border-line bg-panel', className)}>
      {(title || actions) && (
        <header className="flex h-10 items-center justify-between gap-2 border-b border-line px-4">
          <h2 className="text-[12px] font-semibold tracking-wide text-muted uppercase">{title}</h2>
          <div className="flex items-center gap-1">{actions}</div>
        </header>
      )}
      <div className={padded ? 'p-4' : undefined}>{children}</div>
    </section>
  )
}

export function StatusDot({ tone }: { tone: 'online' | 'offline' | 'busy' | 'error' }) {
  return (
    <span
      className={cx(
        'inline-block size-2 shrink-0 rounded-full',
        tone === 'online' && 'bg-accent',
        tone === 'offline' && 'bg-subtle',
        tone === 'busy' && 'bg-warn animate-pulse',
        tone === 'error' && 'bg-danger',
      )}
    />
  )
}

export function Badge({ tone = 'neutral', children }: { tone?: 'neutral' | 'accent' | 'warn' | 'danger'; children: ReactNode }) {
  return (
    <span
      className={cx(
        'inline-flex h-5 items-center gap-1 rounded px-1.5 text-[11px] font-medium',
        tone === 'neutral' && 'bg-elevated text-muted',
        tone === 'accent' && 'bg-accent-soft text-accent',
        tone === 'warn' && 'bg-warn-soft text-warn',
        tone === 'danger' && 'bg-danger-soft text-danger',
      )}
    >
      {children}
    </span>
  )
}

export function ProgressBar({ value, tone = 'accent', className }: { value: number; tone?: 'accent' | 'warn' | 'danger' | 'muted'; className?: string }) {
  const pct = Math.max(0, Math.min(1, value)) * 100
  return (
    <div className={cx('h-1.5 w-full overflow-hidden rounded-full bg-elevated', className)}>
      <div
        className={cx(
          'h-full rounded-full transition-[width] duration-300',
          tone === 'accent' && 'bg-accent',
          tone === 'warn' && 'bg-warn',
          tone === 'danger' && 'bg-danger',
          tone === 'muted' && 'bg-subtle',
        )}
        style={{ width: `${pct}%` }}
      />
    </div>
  )
}

export function EmptyState({ icon, title, children, action }: { icon?: ReactNode; title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 px-6 py-14 text-center">
      {icon && <div className="mb-1 text-subtle">{icon}</div>}
      <div className="text-[14px] font-medium">{title}</div>
      {children && <div className="max-w-sm text-muted">{children}</div>}
      {action && <div className="mt-3">{action}</div>}
    </div>
  )
}

export function ErrorNote({ children }: { children: ReactNode }) {
  if (!children) return null
  return <div className="rounded-md border border-danger/30 bg-danger-soft px-3 py-2 text-[12px] text-danger">{children}</div>
}

export function Modal({ title, open, onClose, children, footer, wide }: {
  title: ReactNode
  open: boolean
  onClose: () => void
  children: ReactNode
  footer?: ReactNode
  wide?: boolean
}) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    ref.current?.querySelector<HTMLElement>('input,button[data-autofocus]')?.focus()
    return () => window.removeEventListener('keydown', onKey)
  }, [open, onClose])
  if (!open) return null
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4" onMouseDown={onClose}>
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        onMouseDown={(e) => e.stopPropagation()}
        className={cx(
          'flex max-h-[90vh] w-full flex-col rounded-lg border border-line bg-panel shadow-2xl shadow-black/40',
          wide ? 'max-w-4xl' : 'max-w-md',
        )}
      >
        <header className="flex h-11 shrink-0 items-center justify-between border-b border-line pr-2 pl-4">
          <h3 className="truncate text-[13px] font-semibold">{title}</h3>
          <IconButton label="Close" onClick={onClose}>
            <X className="size-4" />
          </IconButton>
        </header>
        <div className="min-h-0 flex-1 overflow-auto p-4">{children}</div>
        {footer && <footer className="flex shrink-0 justify-end gap-2 border-t border-line px-4 py-3">{footer}</footer>}
      </div>
    </div>
  )
}

export function PageHeader({ title, subtitle, actions }: { title: string; subtitle?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="flex items-end justify-between gap-4 pb-5">
      <div className="min-w-0">
        <h1 className="text-[20px] font-semibold tracking-tight">{title}</h1>
        {subtitle && <p className="mt-0.5 truncate text-muted">{subtitle}</p>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </div>
  )
}
