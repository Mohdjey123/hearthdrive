/**
 * The last answer to slow requests (a paired device's storage and folders),
 * kept so a page can show it at once, also after the app restarts, while
 * the fresh answer loads. Entries belong to the person signed in, and are
 * dropped when they sign out.
 */

const PREFIX = 'nexus.cache:'
/** Larger answers are kept for this session only. */
const MAX_STORED = 200 * 1024
const MAX_IN_MEMORY = 300

const memory = new Map<string, unknown>()
let scope = ''

function fullKey(key: string) {
  return `${PREFIX}${scope}:${key}`
}

/** Whose cache is in use (the signed-in person). */
export function setCacheScope(person: string) {
  scope = person
}

export function cached<T>(key: string | undefined): T | undefined {
  if (!key || !scope) return undefined
  const k = fullKey(key)
  if (memory.has(k)) return memory.get(k) as T
  try {
    const text = localStorage.getItem(k)
    if (text) {
      const value = JSON.parse(text) as T
      memory.set(k, value)
      return value
    }
  } catch {
    /* storage unavailable or damaged: load fresh */
  }
  return undefined
}

export function remember(key: string | undefined, value: unknown) {
  if (!key || !scope) return
  const k = fullKey(key)
  memory.delete(k)
  memory.set(k, value)
  if (memory.size > MAX_IN_MEMORY) memory.delete(memory.keys().next().value as string)
  try {
    const text = JSON.stringify(value)
    if (text.length <= MAX_STORED) localStorage.setItem(k, text)
    else localStorage.removeItem(k)
  } catch {
    /* full or unavailable: memory only */
  }
}

/** Forgets everything (signing out). */
export function clearCache() {
  memory.clear()
  try {
    for (let i = localStorage.length - 1; i >= 0; i--) {
      const k = localStorage.key(i)
      if (k?.startsWith(PREFIX)) localStorage.removeItem(k)
    }
  } catch {
    /* nothing stored */
  }
}
