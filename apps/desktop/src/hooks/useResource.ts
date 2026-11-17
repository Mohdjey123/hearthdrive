import { useCallback, useEffect, useRef, useState } from 'react'
import { subscribe, type NexusEvent } from '../services/events'

interface Resource<T> {
  data: T | undefined
  error: Error | null
  loading: boolean
  reload: () => void
}

/**
 * Loads data and keeps it fresh. `refreshOn` decides which live events
 * should trigger a (debounced) reload.
 */
export function useResource<T>(
  load: () => Promise<T>,
  deps: unknown[],
  refreshOn?: (e: NexusEvent) => boolean,
): Resource<T> {
  const [data, setData] = useState<T>()
  const [error, setError] = useState<Error | null>(null)
  const [loading, setLoading] = useState(true)
  const seq = useRef(0)
  const loadRef = useRef(load)
  loadRef.current = load
  const inFlight = useRef(false)
  const again = useRef(false)

  const run = useCallback(() => {
    const id = ++seq.current
    inFlight.current = true
    again.current = false
    setLoading(true)
    loadRef
      .current()
      .then((d) => {
        if (id !== seq.current) return
        setData(d)
        setError(null)
      })
      .catch((e: Error) => {
        if (id === seq.current) setError(e)
      })
      .finally(() => {
        if (id !== seq.current) return
        inFlight.current = false
        setLoading(false)
        if (again.current) run()
      })
  }, [])

  // A refresh never cancels a request that is still running: the running
  // one finishes and shows its result, then one more request follows. With
  // a steady stream of events and a slow request, cancelling would mean no
  // result ever arrives.
  const reload = useCallback(() => {
    if (inFlight.current) again.current = true
    else run()
  }, [run])

  // New inputs (another folder, another device) do replace a running request.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(run, deps)

  const filterRef = useRef(refreshOn)
  filterRef.current = refreshOn
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined
    const unsubscribe = subscribe((e) => {
      const f = filterRef.current
      if (e.type === 'resync' || (f && f(e))) {
        // Throttle, not debounce: the first event schedules a refresh and
        // later events do not push it back, so busy storage still refreshes.
        if (timer === undefined) {
          timer = setTimeout(() => {
            timer = undefined
            reload()
          }, 400)
        }
      }
    })
    return () => {
      clearTimeout(timer)
      unsubscribe()
    }
  }, [reload])

  return { data, error, loading, reload }
}
