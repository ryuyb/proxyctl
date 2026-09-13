/**
 * The overview: what the agent is doing right now.
 *
 * # Why this page exists separately from the kernel page
 *
 * The kernel page is where an operator acts — start, stop, reload. This one
 * answers "is anything wrong" without offering a way to make it worse, which is
 * what a page is for when its reader has just opened a tab.
 */

import { useTranslation } from 'react-i18next'
import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'

import {
  CapabilityBadge,
  EmptyState,
  ErrorState,
  LoadingBlock,
  PageBody,
  PageHeader,
  Section,
} from '@/components/page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { get } from '@/lib/api'
import { relative, timestamp } from '@/lib/format'
import { keys, paths } from '@/lib/query'
import { useLanguage } from '@/lib/use-language'
import type { Capabilities, Job, MihomoStatus } from '@/lib/types'

export function OverviewPage() {
  const { t } = useTranslation()
  const language = useLanguage()

  const status = useQuery({
    queryKey: keys.mihomo,
    queryFn: () => get<MihomoStatus>(paths.mihomo),
  })
  const system = useQuery({
    queryKey: keys.system,
    queryFn: () => get<Capabilities>(paths.system),
  })
  const jobs = useQuery({
    queryKey: keys.jobs,
    queryFn: () => get<Job[]>(`${paths.jobs}?limit=5`),
  })

  return (
    <>
      <PageHeader
        title={t('overview.title')}
        description={t('app.tagline')}
        actions={
          <Button variant="outline" size="sm" asChild>
            <Link to="/doctor">{t('doctor.run')}</Link>
          </Button>
        }
      />

      <PageBody>
        {status.isError ? (
          <ErrorState error={status.error} onRetry={() => void status.refetch()} />
        ) : status.isLoading ? (
          <LoadingBlock rows={2} />
        ) : (
          status.data && <KernelCards status={status.data} />
        )}

        <Section
          title={t('overview.capabilities')}
          actions={
            <Button variant="ghost" size="sm" asChild>
              <Link to="/system">{t('nav.system')}</Link>
            </Button>
          }
        >
          {system.isError ? (
            <ErrorState error={system.error} onRetry={() => void system.refetch()} />
          ) : system.isLoading ? (
            <LoadingBlock rows={4} />
          ) : system.data ? (
            <CapabilityGrid capabilities={system.data} />
          ) : null}
        </Section>

        <Section title={t('overview.recentJobs')}>
          {jobs.isError ? (
            <ErrorState error={jobs.error} onRetry={() => void jobs.refetch()} />
          ) : jobs.isLoading ? (
            <LoadingBlock rows={3} />
          ) : !jobs.data?.length ? (
            <EmptyState>{t('common.empty')}</EmptyState>
          ) : (
            <div className="divide-y rounded-lg border">
              {jobs.data.map((job) => (
                <div key={job.id} className="flex items-center justify-between gap-4 px-4 py-2.5">
                  <div className="min-w-0">
                    <div className="truncate text-sm">{job.kind}</div>
                    <div className="truncate text-xs text-muted-foreground">{job.target}</div>
                  </div>
                  <div className="shrink-0 text-right">
                    <Badge variant="outline" className="font-normal">
                      {t(`status.${job.state}`, { defaultValue: job.state })}
                    </Badge>
                    <div
                      className="mt-1 text-xs text-muted-foreground"
                      title={timestamp(job.updated_at, language)}
                    >
                      {relative(job.updated_at, t)}
                    </div>
                  </div>
                </div>
              ))}
            </div>
          )}
        </Section>
      </PageBody>
    </>
  )
}

/** The kernel's headline state. */
function KernelCards({ status }: { status: MihomoStatus }) {
  const { t } = useTranslation()
  const health = status.health

  return (
    <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm font-normal text-muted-foreground">
            {t('overview.kernel')}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-1">
          <div className="text-xl font-semibold">{status.name}</div>
          <div className="text-sm">{t(`status.${status.state.status}`, { defaultValue: status.state.status })}</div>
          {status.build && (
            <div className="font-mono text-xs text-muted-foreground">
              {status.build.version} · {status.build.flavor}
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm font-normal text-muted-foreground">
            {t('overview.activeConfig')}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-1">
          {status.active_config ? (
            <>
              <div className="font-mono text-lg font-semibold">{status.active_config}</div>
              <Link to="/configs" className="text-xs text-muted-foreground underline-offset-4 hover:underline">
                {t('nav.configs')}
              </Link>
            </>
          ) : (
            <div className="text-sm text-muted-foreground">{t('common.none')}</div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm font-normal text-muted-foreground">
            {t('overview.health')}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-1">
          {health ? (
            <>
              <div className="text-lg font-semibold">
                {health.healthy
                  ? t('status.healthy')
                  : health.degraded
                    ? t('status.Degraded')
                    : t('status.unhealthy')}
              </div>
              <div className="text-xs text-muted-foreground">{health.summary}</div>
            </>
          ) : (
            <div className="text-sm text-muted-foreground">{t('overview.noHealth')}</div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm font-normal text-muted-foreground">
            {t('overview.lastFailure')}
          </CardTitle>
        </CardHeader>
        <CardContent>
          {status.last_failure ? (
            <div className="break-words text-sm text-status-error">{status.last_failure}</div>
          ) : (
            <div className="text-sm text-muted-foreground">{t('common.none')}</div>
          )}
        </CardContent>
      </Card>

      {status.state.live && !status.state.serving && (
        // Stated rather than left to be inferred from two badges: the process
        // being alive while nothing is served is the specific state that looks
        // fine on a status page and is not.
        <div className="md:col-span-2 lg:col-span-4">
          <div className="rounded-lg border border-status-warn/40 bg-status-warn/5 px-4 py-3 text-sm">
            {t('status.live')} · {t('status.notServing')}
          </div>
        </div>
      )}
    </div>
  )
}

/** Every probed capability, in the order the agent reported them. */
function CapabilityGrid({ capabilities }: { capabilities: Capabilities }) {
  const { t } = useTranslation()
  return (
    <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
      <div className="rounded-lg border px-4 py-3 sm:col-span-2 lg:col-span-3">
        <dl className="grid grid-cols-2 gap-x-6 gap-y-1 text-xs sm:grid-cols-3 lg:grid-cols-6">
          <Field label={t('system.os')} value={capabilities.environment.os} />
          <Field label={t('system.osVersion')} value={capabilities.environment.os_version} />
          <Field label={t('system.arch')} value={capabilities.environment.arch} />
          <Field label={t('system.kernel')} value={capabilities.environment.kernel} />
          <Field label={t('system.init')} value={capabilities.environment.init} />
          <Field label={t('system.container')} value={capabilities.environment.container} />
        </dl>
      </div>

      {capabilities.capabilities.map((capability) => (
        <div key={capability.kind} className="space-y-1.5 rounded-lg border px-4 py-3">
          <div className="flex items-center justify-between gap-2">
            <span className="font-mono text-sm">{capability.kind}</span>
            <CapabilityBadge status={capability.status} />
          </div>
          {/* The evidence is what makes a capability's state trustworthy rather
           * than asserted: it names the probe and what it saw. */}
          <p className="break-words text-xs text-muted-foreground">{capability.evidence}</p>
        </div>
      ))}
    </div>
  )
}

function Field({ label, value }: { label: string; value: string | null }) {
  return (
    <div>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="truncate font-mono">{value ?? '—'}</dd>
    </div>
  )
}
