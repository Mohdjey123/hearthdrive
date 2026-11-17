import { useSyncExternalStore } from 'react'
import { CheckCircle2, AlertCircle } from 'lucide-react'
import { cx } from './ui'

interface Toast { id: number; message: string; tone: 'success' | 'error' }

let toasts: Toast[] = []
let seq = 0
const listeners = new Set<() => void>()
const emit = () => listeners.forEach((l) => l())

export function toast(message: string, tone: Toast['tone'] = 'success') {
  const id = ++seq
  toasts = [...toasts, { id, message, tone }]
  emit()
  setTimeout(() => {
    toasts = toasts.filter((t) => t.id !== id)
    emit()
  }, tone === 'error' ? 6000 : 3200)
}

export const toastError = (e: unknown) => toast(e instanceof Error ? e.message : String(e), 'error')

export function Toasts() {
  const list = useSyncExternalStore(
    (cb) => {
      listeners.add(cb)
      return () => listeners.delete(cb)
    },
    () => toasts,
  )
  return (
    <div className="pointer-events-none fixed right-4 bottom-4 z-[60] flex w-80 flex-col gap-2">
      {list.map((t) => (
        <div
          key={t.id}
          role="status"
          className={cx(
            'pointer-events-auto flex items-start gap-2 rounded-md border bg-panel px-3 py-2.5 text-[12.5px] shadow-lg shadow-black/30',
            t.tone === 'error' ? 'border-danger/40' : 'border-line',
          )}
        >
          {t.tone === 'error' ? (
            <AlertCircle className="mt-px size-4 shrink-0 text-danger" />
          ) : (
            <CheckCircle2 className="mt-px size-4 shrink-0 text-accent" />
          )}
          <span className="min-w-0 break-words">{t.message}</span>
        </div>
      ))}
    </div>
  )
}
