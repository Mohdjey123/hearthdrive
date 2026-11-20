import { useSyncExternalStore } from 'react'
import { CheckCircle2, AlertCircle, X } from 'lucide-react'
import { Progress, cx } from './ui'

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

/** Something long that runs outside a dialog (a download on the phone), shown
 *  as a card with a progress bar until it ends. */
export interface Activity {
  id: number
  label: string
  done?: number
  total?: number
  bytesDone?: number
  bytesTotal?: number
  noun?: string
  detail?: string
  /** Shown as a close button: hides the card, the work goes on. */
  onHide?: () => void
}

let activities: Activity[] = []
const activityListeners = new Set<() => void>()
const emitActivities = () => activityListeners.forEach((l) => l())

/** Starts an activity card; `update` changes it, `end` takes it away. */
export function startActivity(label: string, start: Omit<Partial<Activity>, 'id' | 'label'> = {}) {
  const id = ++seq
  activities = [...activities, { id, label, ...start }]
  emitActivities()
  return {
    update(changes: Omit<Partial<Activity>, 'id'>) {
      activities = activities.map((a) => (a.id === id ? { ...a, ...changes } : a))
      emitActivities()
    },
    end() {
      activities = activities.filter((a) => a.id !== id)
      emitActivities()
    },
  }
}

export function Toasts() {
  const list = useSyncExternalStore(
    (cb) => {
      listeners.add(cb)
      return () => listeners.delete(cb)
    },
    () => toasts,
  )
  const running = useSyncExternalStore(
    (cb) => {
      activityListeners.add(cb)
      return () => activityListeners.delete(cb)
    },
    () => activities,
  )
  return (
    <div className="pointer-events-none fixed inset-x-4 bottom-20 z-[60] flex flex-col gap-2 md:inset-x-auto md:right-4 md:bottom-4 md:w-80">
      {running.map((a) => (
        <div key={a.id} role="status" className="pointer-events-auto flex items-start gap-1 rounded-md border border-line bg-panel py-2.5 pr-1.5 pl-3 shadow-lg shadow-black/30">
          <Progress
            className="flex-1"
            label={a.label}
            done={a.done}
            total={a.total}
            bytesDone={a.bytesDone}
            bytesTotal={a.bytesTotal}
            noun={a.noun}
            detail={a.detail}
          />
          {a.onHide && (
            <button aria-label="Hide" title="Hide (it keeps going)" onClick={a.onHide} className="nx-icon-btn -mt-1 inline-flex size-6 shrink-0 items-center justify-center rounded text-muted hover:bg-hover hover:text-fg">
              <X className="size-3.5" />
            </button>
          )}
        </div>
      ))}
      {list.map((t) => (
        <div
          key={t.id}
          role={t.tone === 'error' ? 'alert' : 'status'}
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
