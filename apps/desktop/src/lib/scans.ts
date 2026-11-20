/** Scans from the Android app wait in the share inbox under this name. */
const SCAN_NAME = /^Scan \d{4}-\d{2}-\d{2} \d{2}\.\d{2}\.\d{2}( \(\d+\))?\.pdf$/

export const isScan = (name: string) => SCAN_NAME.test(name)

/** Where scans go: a computer, a storage location on it and a folder. */
export interface ScanTarget { device_id: string; device_name: string; root_id: string; path: string }

const KEY = 'nexus.scanTarget'

export function scanTarget(): ScanTarget | null {
  try {
    const t = JSON.parse(localStorage.getItem(KEY) ?? 'null') as ScanTarget | null
    return t && t.device_id && t.root_id ? t : null
  } catch {
    return null
  }
}

export function setScanTarget(t: ScanTarget | null) {
  try {
    if (t) localStorage.setItem(KEY, JSON.stringify(t))
    else localStorage.removeItem(KEY)
  } catch {
    /* storage unavailable: the next scan asks again */
  }
  window.dispatchEvent(new Event('nexus-scan-target'))
}
