/**
 * The kernel's log stream.
 *
 * # Why this is a stream and not a poll
 *
 * A log is a sequence, and the interesting property is order and continuity. A
 * poll would return whatever accumulated between two reads, so a burst would arrive
 * as one clump and a gap between polls would be invisible. The agent already
 * streams, so this consumes the stream.
 *
 * # Why lines are capped
 *
 * A kernel in a reconnect loop can produce thousands of lines a minute. An
 * unbounded buffer would grow until the tab dies, and the lines that matter are
 * almost always the most recent ones. The cap is on *rendered* lines: older ones
 * are dropped, and the fact that they were dropped is stated rather than left to
 * be noticed.
 *
 * # Why a read-only session sees nothing here
 *
 * The agent withholds `mihomo.log` from a read-only session, because a log line
 * describes network topology — hosts, DNS answers, matched rules — even after
 * credentials are stripped. This page says so rather than showing an empty pane
 * that looks broken.
 */

import { useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Pause, Play, Trash2 } from 'lucide-react'

import { EmptyState, PageBody, PageHeader } from '@/components/page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { cn } from '@/lib/utils'
import { paths } from '@/lib/query'
import { useIsAdmin } from '@/lib/session'
import { readLines } from '@/lib/stream'
import type { LogEntry } from '@/lib/types'

/** The levels the agent accepts. `warning` is its own spelling. */
const LEVELS = ['debug', 'info', 'warning', 'error'] as const

/** The most lines held in memory. */
const MAX_LINES = 2_000

/** How long to wait before re-opening a closed stream, in milliseconds. */
const RETRY_DELAY = 2_000

/** A rendered line, with a key that survives filtering. */
interface Line {
  key: number
  entry: LogEntry
}

export function LogsPage() {
  const { t } = useTranslation()
  const isAdmin = useIsAdmin()
  const [level, setLevel] = useState<string>('info')
  const [paused, setPaused] = useState(false)
  const [filter, setFilter] = useState('')
  const [lines, setLines] = useState<Line[]>([])
  const [truncated, setTruncated] = useState(false)
  const [connected, setConnected] = useState(false)
  // Bumped to re-open the stream. A counter rather than a flag, because the effect
  // has to re-run on every increment and setting a boolean to its current value
  // would not.
  const [attempt, setAttempt] = useState(0)

  // The counter gives each line a stable key. An array index is not one: dropping
  // the oldest line would renumber every remaining key and force React to rebuild
  // the whole list on every line.
  const sequence = useRef(0)
  // Read inside the stream callback, which is created once per attempt. A `paused`
  // captured by that closure would be the value from the render that opened the
  // stream, forever. Assigned in an effect rather than during render: a render can
  // be discarded under concurrent rendering, so writing a ref there can leave it
  // holding a value no commit ever produced.
  const pausedRef = useRef(paused)
  useEffect(() => {
    pausedRef.current = paused
  }, [paused])

  useEffect(() => {
    if (!isAdmin) return
    const controller = new AbortController()
    let timer: ReturnType<typeof setTimeout> | undefined

    void readLines<LogEntry>(`${paths.logs}?level=${encodeURIComponent(level)}`, {
      signal: controller.signal,
      onLine: ({ value }) => {
        setConnected(true)
        // A paused page does not buffer. Buffering would mean a pause that lasts a
        // minute produces a minute of lines at once on resume, which is not what
        // pausing is for.
        if (pausedRef.current) return
        if (!value || typeof value.message !== 'string') return
        setLines((previous) => {
          const next = [...previous, { key: sequence.current++, entry: value }]
          if (next.length > MAX_LINES) {
            setTruncated(true)
            return next.slice(next.length - MAX_LINES)
          }
          return next
        })
      },
      onEnd: (reason) => {
        setConnected(false)
        // The agent restarts, and the kernel restarts. Neither should stop this
        // page from being useful, so it reconnects rather than reporting a failure
        // the reader can do nothing about. A fixed delay is right here: the stream
        // carries no state to lose, and re-opening it is one request.
        if (reason !== 'aborted') {
          timer = window.setTimeout(() => setAttempt((value) => value + 1), RETRY_DELAY)
        }
      },
    })

    return () => {
      controller.abort()
      if (timer) window.clearTimeout(timer)
    }
  }, [isAdmin, level, attempt])

  const visible = useMemo(() => {
    const needle = filter.trim().toLowerCase()
    if (!needle) return lines
    return lines.filter((line) => line.entry.message.toLowerCase().includes(needle))
  }, [lines, filter])

  if (!isAdmin) {
    return (
      <>
        <PageHeader title={t('logs.title')} />
        <PageBody>
          <EmptyState>{t('logs.hiddenFromReadOnly')}</EmptyState>
        </PageBody>
      </>
    )
  }

  return (
    <>
      <PageHeader
        title={t('logs.title')}
        description={t('logs.lines', { count: lines.length })}
        actions={
          <>
            <Select value={level} onValueChange={setLevel}>
              <SelectTrigger className="h-8 w-28 text-xs">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {LEVELS.map((value) => (
                  <SelectItem key={value} value={value}>
                    {t(`logs.${value}`, { defaultValue: value })}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPaused((value) => !value)}
            >
              {paused ? <Play className="size-3.5" /> : <Pause className="size-3.5" />}
              {paused ? t('logs.resume') : t('logs.pause')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                setLines([])
                setTruncated(false)
              }}
              disabled={!lines.length}
            >
              <Trash2 className="size-3.5" />
              {t('logs.clear')}
            </Button>
          </>
        }
      />

      <PageBody className="space-y-3">
        <div className="flex flex-wrap items-center gap-3">
          <Input
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
            placeholder={t('logs.filterPlaceholder')}
            className="h-8 max-w-sm text-xs"
          />
          <Badge variant={paused ? 'secondary' : 'outline'} className="font-normal">
            {paused ? t('logs.paused') : connected ? t('logs.follow') : t('logs.disconnected')}
          </Badge>
          {truncated && (
            <span className="text-xs text-muted-foreground">
              {t('logs.lines', { count: MAX_LINES })}
            </span>
          )}
        </div>

        {!visible.length ? (
          <EmptyState>{connected ? t('logs.waiting') : t('common.empty')}</EmptyState>
        ) : (
          <div className="max-h-[calc(100vh-16rem)] overflow-auto rounded-lg border bg-card font-mono text-xs">
            {visible.map((line) => (
              <div
                key={line.key}
                className="flex gap-3 border-b border-border/50 px-3 py-1 last:border-b-0 hover:bg-accent/40"
              >
                <span
                  className={cn(
                    'w-16 shrink-0 uppercase',
                    line.entry.level === 'error' && 'text-status-error',
                    line.entry.level === 'warning' && 'text-status-warn',
                    line.entry.level === 'debug' && 'text-muted-foreground',
                  )}
                >
                  {line.entry.level}
                </span>
                {/* The agent's text verbatim. Redaction happens in its adapter, and
                 * re-processing a log line here would mean two redactors with two
                 * different rules. */}
                <span className="min-w-0 break-all whitespace-pre-wrap">
                  {line.entry.message}
                </span>
              </div>
            ))}
          </div>
        )}
      </PageBody>
    </>
  )
}
