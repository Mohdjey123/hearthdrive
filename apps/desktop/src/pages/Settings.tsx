import { LogOut, Monitor, Moon, Sun } from 'lucide-react'
import { api, connection, isMobile } from '../services/api'
import { useResource } from '../hooks/useResource'
import { useSession } from '../stores/session'
import { useTheme, type ThemePref } from '../stores/theme'
import { formatDuration } from '../lib/format'
import { Badge, Button, PageHeader, Panel, Spinner, cx } from '../components/ui'

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-1 items-start gap-1 py-3 md:grid-cols-[200px_1fr] md:gap-4">
      <div className="text-muted">{label}</div>
      <div className="min-w-0 break-words">{children}</div>
    </div>
  )
}

export function Settings() {
  const { phase, signOut } = useSession()
  const [theme, setTheme] = useTheme()
  const info = useResource(() => api.systemInfo(), [])
  const me = phase.kind === 'ready' ? phase.me : null

  const themes: { value: ThemePref; label: string; icon: typeof Sun }[] = [
    { value: 'dark', label: 'Dark', icon: Moon },
    { value: 'light', label: 'Light', icon: Sun },
    { value: 'system', label: 'System', icon: Monitor },
  ]

  return (
    <div className="max-w-3xl">
      <PageHeader title="Settings" />

      <Panel title="General" className="mb-4">
        <div className="divide-y divide-line">
          <Row label="Theme">
            <div className="inline-flex rounded-md border border-line bg-elevated p-0.5">
              {themes.map(({ value, label, icon: Icon }) => (
                <button
                  key={value}
                  onClick={() => setTheme(value)}
                  className={cx(
                    'flex h-7 items-center gap-1.5 rounded px-3 text-[12.5px]',
                    theme === value ? 'bg-panel text-fg shadow-sm' : 'text-muted hover:text-fg',
                  )}
                >
                  <Icon className="size-3.5" /> {label}
                </button>
              ))}
            </div>
          </Row>
        </div>
      </Panel>

      <Panel title="Account" className="mb-4">
        <div className="divide-y divide-line">
          <Row label="Signed in as">
            <span className="font-medium">{me?.username}</span> <Badge tone="accent">{me?.role}</Badge>
          </Row>
          <Row label="Session">
            {me?.local ? (
              isMobile() ? 'This app signs in to its own node automatically. There is no password to remember.' : 'Desktop app — authenticated automatically on this computer'
            ) : (
              <Button size="sm" icon={<LogOut className="size-3.5" />} onClick={() => void signOut()}>Sign out</Button>
            )}
          </Row>
        </div>
      </Panel>

      <Panel title="Service" className="mb-4">
        {info.data ? (
          <div className="divide-y divide-line">
            <Row label="Node">{info.data.node.name} <span className="text-subtle">({info.data.node.hostname})</span></Row>
            <Row label="Status">Running · up {formatDuration(info.data.uptime_ms)}</Row>
            <Row label="API">
              <span className="font-mono text-[12px]">{connection().baseUrl}/api/v1</span>
            </Row>
            <Row label="Network">{info.data.network.status.display_name} — {info.data.network.status.detail}</Row>
            <Row label="Data folder">
              <span className="font-mono text-[12px]">{info.data.data_dir}</span>
              <div className="mt-1 text-[12px] text-subtle">Database, logs and runtime state. Your files are never stored here.</div>
            </Row>
            <Row label={isMobile() ? 'While the app is closed' : 'Background service'}>
              {isMobile()
                ? 'NEXUS runs on this phone while the app is open. Your computers keep running on their own and are reachable again as soon as you open the app.'
                : 'The storage service keeps running when this window is closed.'}
            </Row>
          </div>
        ) : (
          <Spinner />
        )}
      </Panel>

      <Panel title="About">
        <div className="divide-y divide-line">
          <Row label="Version">NEXUS Cloud {info.data?.node.version ?? ''}</Row>
          <Row label="License">MIT · free, self-hosted, no subscription, no analytics</Row>
        </div>
      </Panel>
    </div>
  )
}
