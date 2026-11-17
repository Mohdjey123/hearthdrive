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

  const reload = useCallback(() => {
    const id = ++seq.current
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
        if (id === seq.current) setLoading(false)
      })
  }, [])

  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(reload, deps)

  const filterRef = useRef(refreshOn)
  filterRef.current = refreshOn
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined
    const unsubscribe = subscribe((e) => {
      const f = filterRef.current
      if (e.type === 'resync' || (f && f(e))) {
        clearTimeout(timer)
        timer = setTimeout(reload, 300)
      }
    })
    return () => {
      clearTimeout(timer)
      unsubscribe()
    }
  }, [reload])

  return { data, error, loading, reload }
}
