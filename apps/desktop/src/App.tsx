import { RefreshCw } from 'lucide-react'
import { useRoute } from './hooks/useRoute'
import { SessionProvider, useSession } from './stores/session'
import { Sidebar, Logo } from './components/Sidebar'
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
  return (
    <div className="flex h-full">
      <Sidebar current={route.page} />
      <main className="min-w-0 flex-1 overflow-auto p-6">
        {route.page === 'overview' && <Overview />}
        {route.page === 'files' && <Files route={route} />}
        {route.page === 'storage' && <Storage route={route} />}
        {route.page === 'devices' && <Devices />}
        {route.page === 'transfers' && <Transfers />}
        {route.page === 'settings' && <Settings />}
      </main>
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
          <div className="text-[15px] font-semibold">Can’t reach the NEXUS service</div>
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
