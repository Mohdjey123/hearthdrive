import { useEffect, useState } from 'react'

export type ThemePref = 'dark' | 'light' | 'system'
const KEY = 'nexus.theme'

function read(): ThemePref {
  try {
    const v = localStorage.getItem(KEY)
    if (v === 'dark' || v === 'light' || v === 'system') return v
  } catch { /* storage unavailable */ }
  return 'dark'
}

function apply(pref: ThemePref) {
  const resolved =
    pref === 'system' ? (window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark') : pref
  document.documentElement.dataset.theme = resolved
}

apply(read())

export function useTheme(): [ThemePref, (p: ThemePref) => void] {
  const [pref, setPref] = useState<ThemePref>(read)
  useEffect(() => {
    apply(pref)
    try { localStorage.setItem(KEY, pref) } catch { /* storage unavailable */ }
    if (pref !== 'system') return
    const mq = window.matchMedia('(prefers-color-scheme: light)')
    const onChange = () => apply('system')
    mq.addEventListener('change', onChange)
    return () => mq.removeEventListener('change', onChange)
  }, [pref])
  return [pref, setPref]
}
