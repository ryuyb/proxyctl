/**
 * The environment, its capabilities, the audit trail, and recent jobs.
 *
 * # Why the audit trail is here and not on its own page
 *
 * It answers "what happened", which is the same question the rest of this page
 * answers about the environment. It is also the only place an operator looks after
 * a refused operation, and making it a page of its own would put it one click
 * further away from the refusal that sent them there.
 *
 * # Why the audit list is not polled
 *
 * It is a record, not a live view. The event stream invalidates it when something
 * is written, and a poll on top of that would re-read a growing table for nothing.
 */

import { useTranslation } from 'react-i18next'
import { useQuery } from '@tanstack/react-query'
import { CheckCircle2, RefreshCw, XCircle } from 'lucide-react'

import {
  CapabilityBadge,
  EmptyState,
  ErrorState,
  LoadingBlock,
  PageBody,
  PageHeader,
  Section,
  StateBadge,
} from '@/components/page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { get } from '@/lib/api'
import { relative, timestamp } from '@/lib/format'
import { keys, paths } from '@/lib/query'
import { useLanguage } from '@/lib/use-language'
import type { Audit, Capabilities, Job } from '@/lib/types'

export function SystemPage() {
  const { t } = useTranslation()
  const language = useLanguage()

  const system = useQuery({
    queryKey: keys.system,
    queryFn: () => get<Capabilities>(paths.system),
  })
  const audit = useQuery({
    queryKey: keys.audit,
    queryFn: () => get<Audit[]>(`${paths.audit}?limit=100`),
  })
  const jobs = useQuery({
    queryKey: keys.jobs,
    queryFn: () => get<Job[]>(`${paths.jobs}?limit=50`),
  })

  return (
    <>
      <PageHeader
        title={t('system.title')}
        actions={
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              void system.refetch()
              void audit.refetch()
              void jobs.refetch()
            }}
          >
            <RefreshCw className="size-3.5" />
            {t('common.refresh')}
          </Button>
        }
      />

      <PageBody>
        <Section title={t('system.environment')}>
          {system.isError ? (
            <ErrorState error={system.error} onRetry={() => void system.refetch()} />
          ) : system.isLoading ? (
            <LoadingBlock rows={2} />
          ) : system.data ? (
            <>
              <div className="grid grid-cols-2 gap-x-6 gap-y-3 rounded-lg border px-4 py-3 sm:grid-cols-3 lg:grid-cols-6">
                <Field label={t('system.os')} value={system.data.environment.os} />
                <Field label={t('system.osVersion')} value={system.data.environment.os_version} />
                <Field label={t('system.arch')} value={system.data.environment.arch} />
                <Field label={t('system.kernel')} value={system.data.environment.kernel} />
                <Field label={t('system.init')} value={system.data.environment.init} />
                <Field label={t('system.container')} value={system.data.environment.container} />
              </div>

              <div className="space-y-2 pt-3">
                <h3 className="text-xs text-muted-foreground">{t('system.capabilities')}</h3>
                <div className="space-y-1.5">
                  {system.data.capabilities.map((capability) => (
                    <div
                      key={capability.kind}
                      className="flex flex-wrap items-center gap-3 rounded-md border px-3 py-2"
                    >
                      <span className="w-40 shrink-0 font-mono text-sm">{capability.kind}</span>
                      <CapabilityBadge status={capability.status} />
                      <span className="min-w-0 flex-1 break-words text-xs text-muted-foreground">
                        {capability.evidence}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            </>
          ) : null}
        </Section>

        <Tabs defaultValue="audit">
          <TabsList>
            <TabsTrigger value="audit">{t('system.audit')}</TabsTrigger>
            <TabsTrigger value="jobs">{t('system.jobs')}</TabsTrigger>
          </TabsList>

          <TabsContent value="audit" className="pt-3">
            {audit.isError ? (
              <ErrorState error={audit.error} onRetry={() => void audit.refetch()} />
            ) : audit.isLoading ? (
              <LoadingBlock rows={6} />
            ) : !audit.data?.length ? (
              <EmptyState>{t('common.empty')}</EmptyState>
            ) : (
              <div className="overflow-hidden rounded-lg border">
                <table className="w-full text-sm">
                  <thead className="bg-muted/50 text-xs text-muted-foreground">
                    <tr>
                      <th className="px-4 py-2 text-left font-normal">{t('system.auditAction')}</th>
                      <th className="px-4 py-2 text-left font-normal">{t('system.auditActor')}</th>
                      <th className="px-4 py-2 text-left font-normal">{t('system.auditTarget')}</th>
                      <th className="px-4 py-2 text-left font-normal">{t('system.auditResult')}</th>
                      <th className="px-4 py-2 text-right font-normal">{t('system.auditAt')}</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y">
                    {audit.data.map((entry) => (
                      <tr key={entry.id}>
                        <td className="px-4 py-2 font-mono text-xs">{entry.action}</td>
                        {/* The actor is already redacted by the domain, so it is
                         * rendered as received rather than being masked again. */}
                        <td className="px-4 py-2 text-xs">{entry.actor}</td>
                        <td className="max-w-0 truncate px-4 py-2 font-mono text-xs">
                          {entry.target}
                        </td>
                        <td className="px-4 py-2">
                          {entry.succeeded ? (
                            <span className="inline-flex items-center gap-1.5 text-xs text-status-ok">
                              <CheckCircle2 className="size-3" />
                              {t('system.auditOk')}
                            </span>
                          ) : (
                            <span
                              className="inline-flex items-center gap-1.5 text-xs text-status-error"
                              title={entry.reason ?? undefined}
                            >
                              <XCircle className="size-3" />
                              {entry.reason ?? t('system.auditFailed')}
                            </span>
                          )}
                        </td>
                        <td
                          className="px-4 py-2 text-right text-xs text-muted-foreground"
                          title={timestamp(entry.at, language)}
                        >
                          {relative(entry.at, t)}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </TabsContent>

          <TabsContent value="jobs" className="pt-3">
            {jobs.isError ? (
              <ErrorState error={jobs.error} onRetry={() => void jobs.refetch()} />
            ) : jobs.isLoading ? (
              <LoadingBlock rows={5} />
            ) : !jobs.data?.length ? (
              <EmptyState>{t('common.empty')}</EmptyState>
            ) : (
              <div className="space-y-2">
                {jobs.data.map((job) => (
                  <div key={job.id} className="space-y-1 rounded-lg border px-4 py-2.5">
                    <div className="flex flex-wrap items-center justify-between gap-3">
                      <div className="flex items-center gap-3">
                        <span className="font-mono text-sm">{job.kind}</span>
                        <Badge variant="outline" className="font-mono text-[10px] font-normal">
                          {job.target}
                        </Badge>
                      </div>
                      <div className="flex items-center gap-3">
                        <StateBadge state={job.state} />
                        <span
                          className="text-xs text-muted-foreground"
                          title={timestamp(job.updated_at, language)}
                        >
                          {relative(job.updated_at, t)}
                        </span>
                      </div>
                    </div>
                    {job.step && (
                      <div className="text-xs text-muted-foreground">
                        {t('system.jobStep')}: {job.step}
                      </div>
                    )}
                    {job.detail && (
                      <div className="break-words text-xs text-muted-foreground">{job.detail}</div>
                    )}
                    {job.degradation && (
                      // A degradation means the job succeeded without doing all of
                      // what was asked. It is highlighted, because "succeeded" on
                      // its own would read as complete.
                      <div className="text-xs text-status-warn">
                        {t('system.jobDegradation')}: {job.degradation}
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </TabsContent>
        </Tabs>
      </PageBody>
    </>
  )
}

function Field({ label, value }: { label: string; value: string | null }) {
  return (
    <div className="min-w-0">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="truncate font-mono text-sm" title={value ?? undefined}>
        {value ?? '—'}
      </div>
    </div>
  )
}
