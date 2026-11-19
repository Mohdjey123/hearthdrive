import { useEffect, useRef, useState } from 'react'
import { Minus, Plus } from 'lucide-react'
import { IconButton, Spinner } from './ui'

type PdfDoc = { numPages: number; getPage: (n: number) => Promise<PdfPage>; destroy: () => Promise<void> }
type PdfPage = {
  getViewport: (o: { scale: number }) => { width: number; height: number }
  render: (o: { canvasContext: CanvasRenderingContext2D; viewport: unknown; canvas: HTMLCanvasElement }) => { promise: Promise<void>; cancel: () => void }
}

/** pdf.js, loaded the first time a PDF is opened (it is large). The legacy
 *  build also runs in older phone web views. */
async function pdfjs() {
  const lib = await import('pdfjs-dist/legacy/build/pdf.mjs')
  const worker = (await import('pdfjs-dist/legacy/build/pdf.worker.min.mjs?url')).default
  lib.GlobalWorkerOptions.workerSrc = worker
  return lib
}

/** Draws one page when it scrolls into view. */
function Page({ doc, n, scale }: { doc: PdfDoc; n: number; scale: number }) {
  const canvas = useRef<HTMLCanvasElement>(null)
  const box = useRef<HTMLDivElement>(null)
  const [size, setSize] = useState<{ w: number; h: number } | null>(null)
  const [visible, setVisible] = useState(n <= 2)

  useEffect(() => {
    let live = true
    doc.getPage(n).then((p) => {
      const v = p.getViewport({ scale })
      if (live) setSize({ w: v.width, h: v.height })
    }).catch(() => {})
    return () => { live = false }
  }, [doc, n, scale])

  useEffect(() => {
    const el = box.current
    if (!el) return
    const io = new IntersectionObserver((e) => e.some((x) => x.isIntersecting) && setVisible(true), { rootMargin: '600px' })
    io.observe(el)
    return () => io.disconnect()
  }, [])

  useEffect(() => {
    if (!visible || !size) return
    let task: { promise: Promise<void>; cancel: () => void } | null = null
    let live = true
    doc.getPage(n).then((p) => {
      const c = canvas.current
      if (!live || !c) return
      const ratio = window.devicePixelRatio || 1
      const v = p.getViewport({ scale: scale * ratio })
      c.width = v.width
      c.height = v.height
      const ctx = c.getContext('2d')
      if (!ctx) return
      task = p.render({ canvasContext: ctx, viewport: v, canvas: c })
      task.promise.catch(() => {})
    }).catch(() => {})
    return () => {
      live = false
      task?.cancel()
    }
  }, [doc, n, scale, visible, size?.w])

  return (
    <div ref={box} className="mx-auto mb-3 bg-white shadow" style={size ? { width: size.w, height: size.h } : { height: 800 }}>
      <canvas ref={canvas} style={size ? { width: size.w, height: size.h } : undefined} />
    </div>
  )
}

export function PdfViewer({ url }: { url: string }) {
  const [doc, setDoc] = useState<PdfDoc | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [scale, setScale] = useState(1.2)

  useEffect(() => {
    let live = true
    let loaded: PdfDoc | null = null
    setDoc(null)
    setError(null)
    pdfjs()
      .then((lib) => lib.getDocument({ url }).promise)
      .then((d) => {
        loaded = d as unknown as PdfDoc
        if (live) setDoc(loaded)
        else void loaded.destroy()
      })
      .catch((e) => live && setError(e?.message ?? 'This PDF could not be opened'))
    return () => {
      live = false
      void loaded?.destroy()
    }
  }, [url])

  if (error) return <div className="text-center text-muted">{error}</div>
  if (!doc) return <Spinner />
  return (
    <div className="flex w-full flex-col">
      <div className="mb-2 flex items-center justify-center gap-2 text-[12.5px] text-muted">
        <IconButton label="Smaller" onClick={() => setScale((s) => Math.max(0.5, s - 0.2))}><Minus className="size-4" /></IconButton>
        {Math.round(scale * 100)}% · {doc.numPages} page{doc.numPages === 1 ? '' : 's'}
        <IconButton label="Larger" onClick={() => setScale((s) => Math.min(3, s + 0.2))}><Plus className="size-4" /></IconButton>
      </div>
      <div className="max-h-[68vh] overflow-auto rounded bg-bg p-3">
        {Array.from({ length: doc.numPages }, (_, i) => <Page key={`${i}-${scale}`} doc={doc} n={i + 1} scale={scale} />)}
      </div>
    </div>
  )
}
