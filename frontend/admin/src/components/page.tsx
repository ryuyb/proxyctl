/**
 * The pieces every page is assembled from.
 *
 * # Why these exist
 *
 * Eight pages each need a heading, a way to say "loading", and a way to say "the
 * agent said no". Written per page, those become eight slightly different
 * renderings of the same three states, and the differences are exactly where a
 * reader's expectations break — one page's error is a red box, another's is a line
 * of text, and a third silently renders nothing.
 *
 * # Why the error state names the cause
 *
 * An unreachable agent and a refused request are the two failures this interface
 * actually sees, and the response to each is different: start the agent, or check
 * your session. Rendering "Something went wrong" for both would make the reader
 * work out which one they have.
 */

import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { AlertTriangle, Loader2, RefreshCw, WifiOff } from 'lucide-react'

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { ApiError } from '@/lib/api'
import { cn } from '@/lib/utils'

/** A page's title area. */
export function PageHeader({
  title,
  description,
  actions,
}: {
  title: ReactNode
  description?: ReactNode
  actions?: ReactNode
}) {
  return (
    <div className="flex items-start justify-between gap-4 border-b px-6 py-5">
      <div className="min-w-0">
        <h1 className="text-lg font-semibold tracking-tight">{title}</h1>
        {description && (
          <div className="mt-1 text-sm text-muted-foreground">{description}</div>
        )}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </div>
  )
}

/** A page's body, with consistent spacing. */
export function PageBody({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn('space-y-6 p-6', className)}>{children}</div>
}

/** A titled section. */
export function Section({
  title,
  actions,
  children,
  className,
}: {
  title: ReactNode
  actions?: ReactNode
  children: ReactNode
  className?: string
}) {
  return (
    <section className={cn('space-y-3', className)}>
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-medium text-muted-foreground">{title}</h2>
        {actions}
      </div>
      {children}
    </section>
  )
}

/** An empty-state message. */
export function EmptyState({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-lg border border-dashed px-4 py-8 text-center text-sm text-muted-foreground">
      {children}
    </div>
  )
}

/** A skeleton for a block of content. */
export function LoadingBlock({ rows = 3 }: { rows?: number }) {
  return (
    <div className="space-y-2">
      {Array.from({ length: rows }, (_, index) => (
        <Skeleton key={index} className="h-9 w-full" />
      ))}
    </div>
  )
}

/** A small inline spinner with a label. */
export function InlineLoading({ label }: { label?: string }) {
  const { t } = useTranslation()
  return (
    <span className="inline-flex items-center gap-2 text-sm text-muted-foreground">
      <Loader2 className="size-3.5 animate-spin" />
      {label ?? t('common.loading')}
    </span>
  )
}

/**
 * A failed read, rendered with advice that matches the failure.
 *
 * `onRetry` is optional: a mutation's failure is not retried by re-running the
 * query, so a retry button there would be misleading.
 */
export function ErrorState({
  error,
  onRetry,
  title,
}: {
  error: unknown
  onRetry?: () => void
  title?: string
}) {
  const { t } = useTranslation()
  const api = error instanceof ApiError ? error : null

  // The three failures call for different actions, so they get different text.
  const heading = api?.isUnreachable
    ? t('common.unreachable')
    : api?.isForbidden
      ? t('common.forbidden')
      : (title ?? t('common.error'))

  const detail = api?.isUnreachable ? t('common.unreachableHint') : (api?.detail ?? String(error))

  return (
    <Alert variant="destructive">
      {api?.isUnreachable ? (
        <WifiOff className="size-4" />
      ) : (
        <AlertTriangle className="size-4" />
      )}
      <AlertTitle>{heading}</AlertTitle>
      <AlertDescription>
        <div className="space-y-3">
          <p className="break-words">{detail}</p>
          {onRetry && (
            <Button variant="outline" size="sm" onClick={onRetry}>
              <RefreshCw className="size-3.5" />
              {t('common.retry')}
            </Button>
          )}
        </div>
      </AlertDescription>
    </Alert>
  )
}

/**
 * The state of something that is one of the five capability states.
 *
 * Never collapsed to a boolean. "Unsupported" — the kernel cannot do it — and
 * "unavailable" — this container was not allowed to — look identical as a red dot
 * and call for completely different responses.
 */
export function CapabilityBadge({ status }: { status: string }) {
  const { t } = useTranslation()
  const variant: Record<string, string> = {
    supported: 'text-status-ok',
    unavailable: 'text-status-warn',
    misconfigured: 'text-status-error',
    unsupported: 'text-muted-foreground',
    unknown: 'text-status-unknown',
  }
  const label = t(`capability.${status}`, { defaultValue: status })
  return (
    <Badge variant="outline" className={cn('gap-1.5 font-normal', variant[status] ?? '')}>
      <span className="size-1.5 rounded-full bg-current" />
      {label}
    </Badge>
  )
}

/** A doctor finding's severity. */
export function SeverityBadge({ severity }: { severity: string }) {
  const { t } = useTranslation()
  const variant: Record<string, 'default' | 'secondary' | 'destructive' | 'outline'> = {
    error: 'destructive',
    warning: 'secondary',
    info: 'outline',
    pass: 'outline',
  }
  return (
    <Badge variant={variant[severity] ?? 'outline'} className="font-normal">
      {t(`severity.${severity}`, { defaultValue: severity })}
    </Badge>
  )
}

/**
 * A lifecycle or job state label.
 *
 * The agent's labels are a closed set this bundle knows, so they are translated;
 * an unfamiliar one is rendered as it arrived rather than hidden, because a state
 * this interface does not recognise is exactly what a reader needs to see.
 */
export function StateBadge({ state }: { state: string }) {
  const { t } = useTranslation()
  const tone: Record<string, string> = {
    running: 'text-status-ok',
    succeeded: 'text-status-ok',
    healthy: 'text-status-ok',
    failed: 'text-status-error',
    degraded: 'text-status-warn',
    stopped: 'text-muted-foreground',
    queued: 'text-muted-foreground',
    // The domain spells lifecycle states capitalised and job states lowercase, so
    // both spellings are listed rather than normalising at render time.
    Running: 'text-status-ok',
    Stopped: 'text-muted-foreground',
    Starting: 'text-status-warn',
    Stopping: 'text-status-warn',
    Failed: 'text-status-error',
  }
  return (
    <span className={cn('inline-flex items-center gap-1.5 text-sm', tone[state] ?? '')}>
      <span className="size-1.5 rounded-full bg-current" />
      {t(`status.${state}`, { defaultValue: state })}
    </span>
  )
}
