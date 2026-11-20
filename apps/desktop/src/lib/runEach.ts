// Runs one request per item, a few at a time, and counts as it goes, so a
// dialog can show "120 of 253 · 14.2 GB of 28.0 GB" while it works.

export interface EachProgress {
  done: number
  total: number
  bytesDone: number
  bytesTotal: number
  /** The item being worked on now. */
  current?: string
}

export interface EachResult<T> {
  ok: T[]
  failed: { item: T; error: Error }[]
  /** Stopped before every item was tried. */
  stopped: boolean
}

export async function runEach<T>(
  items: T[],
  op: (item: T) => Promise<unknown>,
  {
    size = () => 0,
    name,
    onProgress,
    shouldStop,
    parallel = 3,
  }: {
    size?: (item: T) => number
    name?: (item: T) => string
    onProgress: (p: EachProgress) => void
    /** Checked before each item; true leaves the rest untouched. */
    shouldStop?: () => boolean
    parallel?: number
  },
): Promise<EachResult<T>> {
  const p: EachProgress = { done: 0, total: items.length, bytesDone: 0, bytesTotal: items.reduce((s, i) => s + size(i), 0) }
  const result: EachResult<T> = { ok: [], failed: [], stopped: false }
  onProgress({ ...p })
  let next = 0
  const worker = async () => {
    while (next < items.length) {
      if (shouldStop?.()) {
        result.stopped = true
        return
      }
      const item = items[next++]
      if (name) onProgress({ ...p, current: name(item) })
      try {
        await op(item)
        result.ok.push(item)
      } catch (e) {
        result.failed.push({ item, error: e instanceof Error ? e : new Error(String(e)) })
      }
      p.done++
      p.bytesDone += size(item)
      onProgress({ ...p })
    }
  }
  await Promise.all(Array.from({ length: Math.max(1, Math.min(parallel, items.length)) }, worker))
  return result
}
