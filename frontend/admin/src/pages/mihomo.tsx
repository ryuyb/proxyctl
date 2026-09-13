/**
 * The kernel: its lifecycle, its proxy groups, and its nodes.
 *
 * # Why the lifecycle actions confirm
 *
 * Both buttons interrupt traffic. `restart` briefly closes every connection and
 * `stop` closes them until someone notices. A confirmation is the right cost for
 * an action whose consequence lands on every client of the proxy rather than on
 * the person clicking.
 *
 * # Why groups are read-only here
 *
 * Changing a group's selection is a live write into the kernel's own state, and it
 * is what the upstream dashboard is for. Doing it from this page as well would
 * mean two interfaces writing the same field with two different sets of rules
 * about what happens when the kernel reloads underneath them. The page shows what
 * is selected; the dashboard changes it.
 */

import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Play, RefreshCw, RotateCw, Square, Download } from 'lucide-react'

import {
  EmptyState,
  ErrorState,
  InlineLoading,
  LoadingBlock,
  PageBody,
  PageHeader,
  Section,
  StateBadge,
} from '@/components/page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { get, post } from '@/lib/api'
import { delay } from '@/lib/format'
import { keys, paths } from '@/lib/query'
import type { Job, MihomoStatus, Proxies } from '@/lib/types'

/** A lifecycle action, and what it is called. */
type Action = 'start' | 'stop' | 'restart' | 'reload'

export function MihomoPage() {
  const { t } = useTranslation()
  const client = useQueryClient()
  const [confirming, setConfirming] = useState<Action | null>(null)

  const status = useQuery({
    queryKey: keys.mihomo,
    queryFn: () => get<MihomoStatus>(paths.mihomo),
  })
  const proxies = useQuery({
    queryKey: keys.proxies,
    queryFn: () => get<Proxies>(paths.proxies),
  })

  const lifecycle = useMutation({
    mutationFn: (action: Action) => post<Job>(`${paths.mihomo}/${action}`),
    onSuccess: () => {
      setConfirming(null)
      void client.invalidateQueries({ queryKey: keys.mihomo })
      void client.invalidateQueries({ queryKey: keys.jobs })
    },
  })

  const update = useMutation({
    mutationFn: () => post<Job>(paths.kernel),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.kernel })
      void client.invalidateQueries({ queryKey: keys.jobs })
    },
  })

  const run = (action: Action) => {
    // The two destructive actions confirm; `start` and `reload` do not, because
    // neither closes an existing connection and a prompt for them would train the
    // reader to dismiss prompts.
    if (action === 'stop' || action === 'restart') {
      setConfirming(action)
      return
    }
    lifecycle.mutate(action)
  }

  return (
    <>
      <PageHeader
        title={t('mihomo.title')}
        description={status.data ? `${status.data.name} · ${status.data.instance}` : undefined}
        actions={
          <Button
            variant="outline"
            size="sm"
            onClick={() => void status.refetch()}
            disabled={status.isFetching}
          >
            <RefreshCw className={status.isFetching ? 'size-3.5 animate-spin' : 'size-3.5'} />
            {t('common.refresh')}
          </Button>
        }
      />

      <PageBody>
        {status.isError ? (
          <ErrorState error={status.error} onRetry={() => void status.refetch()} />
        ) : status.isLoading ? (
          <LoadingBlock rows={2} />
        ) : (
          status.data && (
            <div className="grid gap-4 lg:grid-cols-3">
              <Card className="lg:col-span-2">
                <CardContent className="space-y-4 pt-6">
                  <div className="flex flex-wrap items-center gap-x-8 gap-y-3">
                    <div>
                      <div className="text-xs text-muted-foreground">{t('mihomo.lifecycle')}</div>
                      <div className="mt-1">
                        <StateBadge state={status.data.state.status} />
                      </div>
                    </div>
                    {status.data.build && (
                      <div>
                        <div className="text-xs text-muted-foreground">
                          {t('mihomo.kernelVersion')}
                        </div>
                        <div className="mt-1 font-mono text-sm">
                          {status.data.build.version}
                          <span className="ml-2 text-muted-foreground">
                            {status.data.build.flavor}
                          </span>
                        </div>
                      </div>
                    )}
                    {status.data.active_config && (
                      <div>
                        <div className="text-xs text-muted-foreground">
                          {t('overview.activeConfig')}
                        </div>
                        <div className="mt-1 font-mono text-sm">{status.data.active_config}</div>
                      </div>
                    )}
                  </div>

                  <div className="flex flex-wrap gap-2">
                    <Button size="sm" onClick={() => run('start')} disabled={lifecycle.isPending}>
                      <Play className="size-3.5" />
                      {t('mihomo.start')}
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => run('reload')}
                      disabled={lifecycle.isPending}
                    >
                      <RotateCw className="size-3.5" />
                      {t('mihomo.reload')}
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => run('restart')}
                      disabled={lifecycle.isPending}
                    >
                      <RefreshCw className="size-3.5" />
                      {t('mihomo.restart')}
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => run('stop')}
                      disabled={lifecycle.isPending}
                    >
                      <Square className="size-3.5" />
                      {t('mihomo.stop')}
                    </Button>
                  </div>

                  {lifecycle.isPending && <InlineLoading />}
                  {lifecycle.isError && <ErrorState error={lifecycle.error} />}
                </CardContent>
              </Card>

              <Card>
                <CardContent className="space-y-3 pt-6">
                  <div className="text-xs text-muted-foreground">{t('mihomo.kernelVersion')}</div>
                  <Button
                    size="sm"
                    variant="outline"
                    className="w-full"
                    onClick={() => update.mutate()}
                    disabled={update.isPending}
                  >
                    <Download className="size-3.5" />
                    {update.isPending ? t('mihomo.updating') : t('mihomo.install')}
                  </Button>
                  {update.isError && <ErrorState error={update.error} />}
                  {status.data.last_failure && (
                    <p className="break-words text-xs text-status-error">
                      {status.data.last_failure}
                    </p>
                  )}
                </CardContent>
              </Card>
            </div>
          )
        )}

        <Section title={t('mihomo.proxyGroups')}>
          {proxies.isError ? (
            <ErrorState error={proxies.error} onRetry={() => void proxies.refetch()} />
          ) : proxies.isLoading ? (
            <LoadingBlock rows={4} />
          ) : !proxies.data?.groups.length ? (
            <EmptyState>{t('common.empty')}</EmptyState>
          ) : (
            <div className="grid gap-3 lg:grid-cols-2">
              {proxies.data.groups.map((group) => (
                <div key={group.name} className="space-y-2 rounded-lg border px-4 py-3">
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate font-medium">{group.name}</span>
                    <Badge variant="outline" className="shrink-0 font-mono font-normal">
                      {group.kind}
                    </Badge>
                  </div>
                  <div className="flex items-center gap-2 text-sm">
                    <span className="text-muted-foreground">{t('mihomo.groupNow')}:</span>
                    <span className="truncate font-mono">{group.now ?? t('common.none')}</span>
                  </div>
                  <div className="text-xs text-muted-foreground">
                    {t('mihomo.memberCount', { count: group.members.length })}
                  </div>
                </div>
              ))}
            </div>
          )}
        </Section>

        <Section title={t('mihomo.nodes')}>
          {proxies.isLoading ? (
            <LoadingBlock rows={5} />
          ) : !proxies.data?.proxies.length ? (
            <EmptyState>{t('common.empty')}</EmptyState>
          ) : (
            <div className="overflow-hidden rounded-lg border">
              <table className="w-full text-sm">
                <thead className="bg-muted/50 text-xs text-muted-foreground">
                  <tr>
                    <th className="px-4 py-2 text-left font-normal">{t('mihomo.nodes')}</th>
                    <th className="px-4 py-2 text-left font-normal">{t('mihomo.nodeKind')}</th>
                    <th className="px-4 py-2 text-right font-normal">{t('mihomo.nodeDelay')}</th>
                  </tr>
                </thead>
                <tbody className="divide-y">
                  {proxies.data.proxies.map((proxy) => (
                    <tr key={proxy.name}>
                      <td className="max-w-0 truncate px-4 py-2 font-mono">{proxy.name}</td>
                      <td className="px-4 py-2">
                        <Badge variant="outline" className="font-mono font-normal">
                          {proxy.kind}
                        </Badge>
                      </td>
                      <td className="px-4 py-2 text-right font-mono text-muted-foreground">
                        {delay(proxy.delay_millis, t)}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </Section>
      </PageBody>

      <ConfirmDialog
        open={confirming !== null}
        onOpenChange={(open) => !open && setConfirming(null)}
        title={t('common.confirmTitle')}
        description={
          confirming === 'stop' ? t('mihomo.confirmStop') : t('mihomo.confirmRestart')
        }
        confirmLabel={t('common.confirm')}
        cancelLabel={t('common.cancel')}
        pending={lifecycle.isPending}
        onConfirm={() => confirming && lifecycle.mutate(confirming)}
      />
    </>
  )
}
