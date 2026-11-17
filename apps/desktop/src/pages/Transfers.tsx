import { ArrowUpDown, CheckCircle2, RotateCcw, Upload, X, XCircle } from 'lucide-react'
import { cancelTransfer, clearFinished, retryTransfer, useTransfers, type Transfer } from '../stores/transfers'
import { formatBytes, formatDuration } from '../lib/format'
import { Badge, Button, EmptyState, IconButton, PageHeader, ProgressBar } from '../components/ui'

function StatusBadge({ t }: { t: Transfer }) {
  switch (t.status) {
    case 'queued': return <Badge>Queued</Badge>
    case 'uploading': return <Badge tone="accent">Uploading</Badge>
    case 'done': return <Badge tone="accent"><CheckCircle2 className="size-3" /> Done</Badge>
    case 'cancelled': return <Badge>Cancelled</Badge>
    case 'error': return <Badge tone="danger"><XCircle className="size-3" /> Failed</Badge>
  }
}

export function Transfers() {
  const transfers = useTransfers()
  const finished = transfers.some((t) => t.status === 'done' || t.status === 'error' || t.status === 'cancelled')

  return (
    <div>
      <PageHeader
        title="Transfers"
        subtitle="Uploads stream straight to the destination drive and are verified with BLAKE3"
        actions={finished && <Button onClick={clearFinished}>Clear finished</Button>}
      />
      <div className="rounded-lg border border-line bg-panel">
        {transfers.length === 0 ? (
          <EmptyState icon={<ArrowUpDown className="size-8" />} title="No transfers">
            Uploads you start from Files appear here with progress and speed.
          </EmptyState>
        ) : (
          <ul className="divide-y divide-line">
            {transfers.map((t) => {
              const pct = t.size ? t.loaded / t.size : t.status === 'done' ? 1 : 0
              const remaining = t.speed > 0 ? ((t.size - t.loaded) / t.speed) * 1000 : 0
              return (
                <li key={t.id} className="flex items-center gap-3 px-4 py-3 md:gap-4 md:px-5">
                  <Upload className="size-4 shrink-0 text-muted" />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="truncate font-medium">{t.name}</span>
                      <StatusBadge t={t} />
                    </div>
                    <div className="mt-0.5 truncate text-[12px] text-subtle">
                      to {t.rootName}{t.path ? ` / ${t.path}` : ''}
                      {t.error && <span className="text-danger"> · {t.error}</span>}
                    </div>
                    {(t.status === 'uploading' || t.status === 'queued') && (
                      <ProgressBar className="mt-2" value={pct} />
                    )}
                  </div>
                  <div className="w-24 shrink-0 text-right text-[12px] text-muted tabular md:w-44">
                    <div>{formatBytes(t.loaded)} / {formatBytes(t.size)}</div>
                    {t.status === 'uploading' && t.speed > 0 && (
                      <div className="text-subtle">{formatBytes(t.speed)}/s · {formatDuration(remaining)} left</div>
                    )}
                  </div>
                  <div className="w-8 shrink-0">
                    {(t.status === 'uploading' || t.status === 'queued') && (
                      <IconButton label="Cancel" onClick={() => cancelTransfer(t.id)}><X className="size-4" /></IconButton>
                    )}
                    {(t.status === 'error' || t.status === 'cancelled') && (
                      <IconButton label="Retry" onClick={() => retryTransfer(t.id)}><RotateCcw className="size-4" /></IconButton>
                    )}
                  </div>
                </li>
              )
            })}
          </ul>
        )}
      </div>
    </div>
  )
}
