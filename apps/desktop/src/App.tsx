import { RefreshCw } from 'lucide-react'
import { useRoute } from './hooks/useRoute'
import { SessionProvider, useSession } from './stores/session'
import { BottomNav, Sidebar, Logo } from './components/Sidebar'
import { isMobile } from './services/api'
import { Toasts } from './components/Toasts'
import { Button, Spinner } from './components/ui'
import { Overview } from './pages/Overview'
import { Files } from './pages/Files'
import { Storage } from './pages/Storage'
import { Devices } from './pages/Devices'
import { Transfers } from './pages/Transfers'
import { Settings } from './pages/Settings'
import { Setup } from './pages/Setup'
import { Login } from './pages/Login'

function Shell() {
  const route = useRoute()
  // A phone holds no storage of its own: it opens on the files of the
  // computers it is paired with.
  const page = isMobile() && (route.page === 'overview' || route.page === 'storage') ? 'files' : route.page
  return (
    <div className="safe-area flex h-full flex-col md:flex-row">
      <Sidebar current={page} />
      <main className="min-h-0 min-w-0 flex-1 overflow-auto p-4 md:p-6">
        {page === 'overview' && <Overview />}
        {page === 'files' && <Files route={route} />}
        {page === 'storage' && <Storage route={route} />}
        {page === 'devices' && <Devices />}
        {page === 'transfers' && <Transfers />}
        {page === 'settings' && <Settings />}
      </main>
      <BottomNav current={page} />
    </div>
  )
}

function Root() {
  const { phase, reload } = useSession()
  switch (phase.kind) {
    case 'loading':
      return (
        <div className="flex h-full flex-col items-center justify-center gap-4">
          <Logo className="size-10" />
          <Spinner />
        </div>
      )
    case 'error':
      return (
        <div className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
          <Logo className="size-10" />
          <div className="text-[15px] font-semibold">{isMobile() ? "NEXUS couldn’t start" : "Can’t reach the NEXUS service"}</div>
          <p className="max-w-md text-muted">{phase.message}</p>
          <Button icon={<RefreshCw className="size-3.5" />} onClick={() => void reload()}>Try again</Button>
        </div>
      )
    case 'setup':
      return <Setup defaultName={phase.nodeName} />
    case 'login':
      return <Login nodeName={phase.nodeName} />
    case 'ready':
      return <Shell />
  }
}

export function App() {
  return (
    <SessionProvider>
      <Root />
      <Toasts />
    </SessionProvider>
  )
}
