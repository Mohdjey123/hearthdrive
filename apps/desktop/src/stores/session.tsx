import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from 'react'
import { clearCache, setCacheScope } from '../lib/cache'
import { api, ApiError, initConnection, onUnauthorized, setSessionToken, type Me } from '../services/api'
import { startEvents, stopEvents } from '../services/events'

type Phase =
  | { kind: 'loading' }
  | { kind: 'error'; message: string }
  | { kind: 'setup'; nodeName: string }
  | { kind: 'login'; nodeName: string }
  | { kind: 'ready'; me: Me; nodeName: string }

interface SessionContext {
  phase: Phase
  reload: () => Promise<void>
  signIn: (token: string) => Promise<void>
  signOut: () => Promise<void>
  setNodeName: (name: string) => void
}

const Ctx = createContext<SessionContext | null>(null)

export function SessionProvider({ children }: { children: ReactNode }) {
  const [phase, setPhase] = useState<Phase>({ kind: 'loading' })

  const reload = useCallback(async () => {
    setPhase({ kind: 'loading' })
    try {
      const conn = await initConnection()
      const status = await api.setupStatus()
      if (status.needs_setup) {
        setPhase({ kind: 'setup', nodeName: status.node_name })
        return
      }
      if (!conn.token) {
        setPhase({ kind: 'login', nodeName: status.node_name })
        return
      }
      try {
        const me = await api.me()
        setCacheScope(me.username)
        setPhase({ kind: 'ready', me, nodeName: status.node_name })
        startEvents()
      } catch (e) {
        if (e instanceof ApiError && e.status === 401) {
          setSessionToken(null)
          setPhase({ kind: 'login', nodeName: status.node_name })
        } else throw e
      }
    } catch (e) {
      setPhase({ kind: 'error', message: e instanceof Error ? e.message : String(e) })
    }
  }, [])

  useEffect(() => {
    void reload()
    onUnauthorized(() => {
      stopEvents()
      setSessionToken(null)
      setPhase((p) => ('nodeName' in p ? { kind: 'login', nodeName: p.nodeName } : p))
    })
  }, [reload])

  const signIn = useCallback(async (token: string) => {
    setSessionToken(token)
    await reload()
  }, [reload])

  const signOut = useCallback(async () => {
    try { await api.logout() } catch { /* already gone */ }
    stopEvents()
    setSessionToken(null)
    clearCache()
    await reload()
  }, [reload])

  const setNodeName = useCallback((nodeName: string) => {
    setPhase((p) => (p.kind === 'ready' ? { ...p, nodeName } : p))
  }, [])

  return <Ctx.Provider value={{ phase, reload, signIn, signOut, setNodeName }}>{children}</Ctx.Provider>
}

export function useSession() {
  const ctx = useContext(Ctx)
  if (!ctx) throw new Error('useSession outside SessionProvider')
  return ctx
}
