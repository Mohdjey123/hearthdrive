import type { Job } from '../services/api'
import { Progress } from './ui'

const WORDS: Record<string, string> = {
  queued: 'Waiting for its turn',
  paused: 'Paused',
}

/** A node job's progress, for showing it next to what it works on. */
export function JobProgress({ job, label, className }: { job: Job; label?: string; className?: string }) {
  const waiting = job.status !== 'running'
  return (
    <Progress
      className={className}
      label={label ?? job.title}
      tone={job.status === 'paused' ? 'warn' : 'accent'}
      done={job.files_done}
      total={job.files_total || undefined}
      bytesDone={job.bytes_done}
      bytesTotal={job.bytes_total || undefined}
      noun="files"
      detail={waiting ? job.note ?? WORDS[job.status] : job.current ?? 'Getting ready…'}
    />
  )
}
