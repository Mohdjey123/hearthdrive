import { useEffect } from 'react'
import { RefreshCw } from 'lucide-react'
import { useRoute } from './hooks/useRoute'
import { SessionProvider, useSession } from './stores/session'
import { BottomNav, Sidebar, Logo, pageAvailable } from './components/Sidebar'
import { androidBridge, isMobile } from './services/api'
import { Toasts } from './components/Toasts'
import { Button, Spinner } from './components/ui'
import { Overview } from './pages/Overview'
import { Files } from './pages/Files'
import { Duplicates } from './pages/Duplicates'
import { Automation } from './pages/Automation'
import { Plugins } from './pages/Plugins'
import { Photos } from './pages/Photos'
import { Media } from './pages/Media'
import { Documents } from './pages/Documents'
import { Storage } from './pages/Storage'
import { Devices } from './pages/Devices'
import { Transfers } from './pages/Transfers'
import { Sync } from './pages/Sync'
import { Backup } from './pages/Backup'
import { Backups } from './pages/Backups'
import { Copies } from './pages/Copies'
import { Health } from './pages/Health'
import { Shares } from './pages/Shares'
import { ShareInbox } from './components/ShareInbox'
import { ScanInbox } from './components/ScanInbox'
import { UpdateBanner } from './components/Updates'
import { TitleBar, hasTitleBar } from './components/TitleBar'
import { Settings } from './pages/Settings'
import { Terminal } from './pages/Terminal'
import { Setup } from './pages/Setup'
import { Login } from './pages/Login'

/** Opens the page a home-screen shortcut points at (Android app). */
function useLaunchRoute() {
  useEffect(() => {
    const bridge = androidBridge()
    if (!bridge?.takeLaunchRoute) return
    const go = () => {
      const route = bridge.takeLaunchRoute?.()
      if (route) window.location.hash = `/${route}`
    }
    go()
    window.addEventListener('nexus-route', go)
    return () => window.removeEventListener('nexus-route', go)
  }, [])
}

function Shell() {
  const route = useRoute()
  useLaunchRoute()
  // A phone holds no storage of its own: it opens on the files of the
  // computers it is paired with.
  const page = pageAvailable(route.page) ? route.page : isMobile() ? 'files' : 'overview'
  return (
    <div className="safe-area flex h-full flex-col md:flex-row">
      <Sidebar current={page} />
      <main className="min-h-0 min-w-0 flex-1 overflow-auto overscroll-contain p-4 md:p-6">
        <UpdateBanner />
        {page === 'overview' && <Overview />}
        {page === 'files' && <Files route={route} />}
        {page === 'duplicates' && <Duplicates route={route} />}
        {page === 'automation' && <Automation />}
        {page === 'plugins' && <Plugins />}
        {page === 'photos' && <Photos route={route} />}
        {page === 'media' && <Media route={route} />}
        {page === 'documents' && <Documents route={route} />}
        {page === 'storage' && <Storage route={route} />}
        {page === 'devices' && <Devices />}
        {page === 'sync' && <Sync />}
        {page === 'backups' && <Backups />}
        {page === 'copies' && <Copies />}
        {page === 'health' && <Health />}
        {page === 'backup' && <Backup />}
        {page === 'shares' && <Shares />}
        {page === 'transfers' && <Transfers />}
        {page === 'settings' && <Settings />}
        {page === 'terminal' && <Terminal route={route} />}
      </main>
      <BottomNav current={page} />
      {isMobile() && <ShareInbox />}
      {isMobile() && <ScanInbox />}
    </div>
  )
}

function Root() {
  const { phase, reload } = useSession()
  switch (phase.kind) {
    case 'loading':
      return (
        <div className="flex h-full flex-col items-center justify-center gap-4">
          <Logo className="size-10 text-accent" />
          <Spinner />
        </div>
      )
    case 'error':
      return (
        <div className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
          <Logo className="size-10 text-accent" />
          <div className="text-[15px] font-semibold">{isMobile() ? "HearthDrive couldn’t start" : "Can’t reach the HearthDrive service"}</div>
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
      {hasTitleBar() ? (
        <div className="flex h-full flex-col">
          <TitleBar />
          <div className="min-h-0 flex-1">
            <Root />
          </div>
        </div>
      ) : (
        <Root />
      )}
      <Toasts />
    </SessionProvider>
  )
}
