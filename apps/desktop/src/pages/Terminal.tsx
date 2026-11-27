import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { ArrowLeft, RotateCcw, Square } from 'lucide-react'
import { ApiError, api, terminalApi, type TerminalChunk, type TerminalSession } from '../services/api'
import { useResource } from '../hooks/useResource'
import { navigate, type Route } from '../hooks/useRoute'
import { toastError } from '../components/Toasts'
import { Button, ErrorNote, PageHeader, Spinner, cx } from '../components/ui'

/** Output kept on screen; older lines scroll away. */
const KEEP = 3000

const KIND: Record<TerminalChunk['kind'], string> = {
  out: '',
  err: 'text-[#ff8a80]',
  in: 'text-[#9cdcfe]',
  info: 'italic text-[#a0a0a0]',
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e))
const pause = (ms: number) => new Promise((r) => setTimeout(r, ms))

/** A shell on a paired device (`device` param), or on this one. Lines typed
 *  here run there, and what they print comes back as it appears. */
export function Terminal({ route }: { route: Route }) {
  const deviceId = route.params.get('device') ?? ''
  const devices = useResource(() => api.devices(), [])
  const device = devices.data?.find((d) => d.id === deviceId)
  const name = deviceId ? (device?.name ?? 'the other device') : 'this device'
  const t = useMemo(() => terminalApi(deviceId), [deviceId])

  const [session, setSession] = useState<Omit<TerminalSession, 'chunks'> | null>(null)
  const [lines, setLines] = useState<TerminalChunk[]>([])
  const [error, setError] = useState('')
  const [starting, setStarting] = useState(false)
  const [text, setText] = useState('')
  const history = useRef<string[]>([])
  const back = useRef(-1)
  const screen = useRef<HTMLDivElement>(null)
  const input = useRef<HTMLTextAreaElement>(null)
  const pinned = useRef(true)
  // Each start bumps this, so an older session's reads stop.
  const generation = useRef(0)
  const current = useRef<string | null>(null)

  const follow = useCallback(async (id: string, after: number, mine: number) => {
    while (generation.current === mine) {
      try {
        const s = await t.read(id, after)
        if (generation.current !== mine) return
        after = s.next
        const { chunks, ...rest } = s
        if (chunks.length) setLines((prev) => [...prev, ...chunks].slice(-KEEP))
        setSession(rest)
        setError('')
        if (s.closed) return
      } catch (e) {
        if (generation.current !== mine) return
        // Ended, or no longer allowed (turned off on the device, access changed).
        if (e instanceof ApiError && (e.status === 404 || e.status === 403)) {
          setSession((prev) => prev && { ...prev, busy: false, closed: e.status === 404 ? 'This terminal has ended.' : e.message })
          return
        }
        setError(message(e))
        await pause(2000)
      }
    }
  }, [t])

  const start = useCallback(async (cwd?: string) => {
    const mine = ++generation.current
    setStarting(true)
    setError('')
    try {
      const s = await t.open(cwd)
      if (generation.current !== mine) {
        void t.close(s.id).catch(() => {})
        return
      }
      current.current = s.id
      const { chunks, ...rest } = s
      setLines((prev) => (cwd ? [...prev, ...chunks] : chunks).slice(-KEEP))
      setSession(rest)
      void follow(s.id, s.next, mine)
    } catch (e) {
      setError(message(e))
    } finally {
      setStarting(false)
    }
  }, [t, follow])

  useEffect(() => {
    setLines([])
    setSession(null)
    void start()
    return () => {
      generation.current++
      const id = current.current
      current.current = null
      if (id) void t.close(id).catch(() => {})
    }
  }, [t, start])

  // Stay at the bottom while new output arrives, unless scrolled up.
  useLayoutEffect(() => {
    const el = screen.current
    if (el && pinned.current) el.scrollTop = el.scrollHeight
  }, [lines, session?.busy])

  useEffect(() => {
    if (session && !session.closed) input.current?.focus({ preventScroll: true })
  }, [session?.id, session?.closed])

  const send = async () => {
    if (!session || session.closed) return
    const value = text
    setText('')
    back.current = -1
    if (/^\s*(cls|clear)\s*$/i.test(value)) {
      setLines([])
      return
    }
    if (value.trim() && history.current[history.current.length - 1] !== value) history.current.push(value)
    pinned.current = true
    try {
      await t.input(session.id, value)
    } catch (e) {
      toastError(e)
    }
  }

  const stop = async () => {
    if (!session) return
    try {
      await t.stop(session.id)
    } catch (e) {
      toastError(e)
    }
  }

  const onKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      void send()
    } else if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && !text.includes('\n')) {
      const h = history.current
      if (!h.length) return
      e.preventDefault()
      let i = back.current === -1 ? h.length : back.current
      i = e.key === 'ArrowUp' ? Math.max(0, i - 1) : i + 1
      if (i >= h.length) {
        back.current = -1
        setText('')
      } else {
        back.current = i
        setText(h[i])
      }
    } else if (e.key === 'c' && e.ctrlKey && input.current?.selectionStart === input.current?.selectionEnd) {
      e.preventDefault()
      void stop()
    } else if (e.key === 'l' && e.ctrlKey) {
      e.preventDefault()
      setLines([])
    }
  }

  const prompt = session ? (session.shell === 'PowerShell' ? `PS ${session.cwd}> ` : `${session.cwd} $ `) : ''
  const open = session && !session.closed

  return (
    <div className="flex h-full min-h-0 flex-col">
      <PageHeader
        title="Terminal"
        subtitle={session ? `${session.shell} on ${name}` : `On ${name}`}
        actions={
          <>
            <Button size="sm" variant="ghost" icon={<ArrowLeft className="size-3.5" />} onClick={() => navigate('devices')}>Devices</Button>
            {open && (
              <Button size="sm" icon={<Square className="size-3.5" />} onClick={() => void stop()} title="Ends the terminal and whatever it is running (Ctrl+C)">
                Stop
              </Button>
            )}
          </>
        }
      />
      {error && <div className="mb-3"><ErrorNote>{error}</ErrorNote></div>}
      <div
        ref={screen}
        onScroll={(e) => {
          const el = e.currentTarget
          pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40
        }}
        onClick={() => {
          if (!window.getSelection()?.toString()) input.current?.focus({ preventScroll: true })
        }}
        className="min-h-[240px] flex-1 overflow-auto rounded-lg border border-line bg-[#101010] p-3 font-mono text-[12.5px] leading-[1.5] text-[#e8e8e8] [overflow-wrap:anywhere] whitespace-pre-wrap"
      >
        {!session && starting && <Spinner />}
        {lines.map((c, i) => (
          <span key={i} className={KIND[c.kind]}>{c.text}</span>
        ))}
        {open && (
          <div className="flex items-start">
            {!session.busy && <span className="shrink-0 whitespace-pre text-[#9cdcfe]">{prompt}</span>}
            <textarea
              ref={input}
              rows={Math.min(8, text.split('\n').length)}
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={onKey}
              aria-label={`Command for ${name}`}
              autoCapitalize="off"
              autoCorrect="off"
              autoComplete="off"
              spellCheck={false}
              enterKeyHint="send"
              className="min-w-0 flex-1 resize-none bg-transparent p-0 font-mono text-[12.5px] leading-[1.5] text-[#e8e8e8] caret-[#e8e8e8] !outline-none focus:!ring-0 focus-visible:!outline-none"
            />
          </div>
        )}
        {open && session.busy && <span className="mt-1 block text-[11px] text-[#a0a0a0]">Running… type to answer it, or press Stop.</span>}
      </div>
      {session?.closed && (
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <span className="text-muted">{session.closed}</span>
          <Button size="sm" icon={<RotateCcw className="size-3.5" />} loading={starting} onClick={() => void start(session.cwd)}>
            Start again
          </Button>
        </div>
      )}
      <p className={cx('mt-2 text-[11.5px] text-subtle', session?.closed && 'hidden')}>
        Enter runs a line, Shift+Enter adds another, ↑ and ↓ bring back earlier ones. Programs that draw a full screen or ask for a password don't work here.
      </p>
    </div>
  )
}
