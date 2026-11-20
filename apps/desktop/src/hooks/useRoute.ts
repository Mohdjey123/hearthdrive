import { useEffect, useState } from 'react'

export type Page = 'overview' | 'files' | 'photos' | 'media' | 'documents' | 'duplicates' | 'automation' | 'storage' | 'health' | 'devices' | 'sync' | 'copies' | 'backups' | 'backup' | 'shares' | 'transfers' | 'settings'
const PAGES: Page[] = ['overview', 'files', 'photos', 'media', 'documents', 'duplicates', 'automation', 'storage', 'health', 'devices', 'sync', 'copies', 'backups', 'backup', 'shares', 'transfers', 'settings']

export interface Route {
  page: Page
  params: URLSearchParams
}

function parse(): Route {
  const raw = window.location.hash.replace(/^#\/?/, '')
  const [name, query = ''] = raw.split('?')
  const page = (PAGES as string[]).includes(name) ? (name as Page) : 'overview'
  return { page, params: new URLSearchParams(query) }
}

export function navigate(page: Page, params: Record<string, string | undefined> = {}) {
  const s = new URLSearchParams()
  for (const [k, v] of Object.entries(params)) if (v) s.set(k, v)
  const query = s.toString()
  window.location.hash = `/${page}${query ? `?${query}` : ''}`
}

export function useRoute(): Route {
  const [route, setRoute] = useState(parse)
  useEffect(() => {
    const onChange = () => setRoute(parse())
    window.addEventListener('hashchange', onChange)
    return () => window.removeEventListener('hashchange', onChange)
  }, [])
  return route
}
