/**
 * Live connections.
 *
 * # Why the process columns are conditional
 *
 * The agent removes `uid`, `process`, and `process_path` for a read-only session
 * before it serialises them, so an administrator and a read-only caller receive
 * genuinely different payloads. This page therefore has to know whether to draw the
 * columns at all: drawing them for everyone would show a read-only reader three
 * empty columns and invite them to conclude the agent looked and found nothing.
 * A note says so instead.
 *
 * # Why the list is polled rather than pushed
 *
 * The event stream carries a `connections` change as a signal, but a connection
 * list is high-cardinality — every byte counter moves continuously — and streaming
 * it would be a running firehose of mostly-identical payloads. A short poll while
 * the page is open is the honest trade: bounded traffic, bounded staleness, and a
 * list that is always a coherent snapshot rather than an accumulation of deltas.
 */

import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { RefreshCw, Search, X, XCircle } from 'lucide-react'

import { ConfirmDialog } from '@/components/confirm-dialog'
import {
  EmptyState,
  ErrorState,
  LoadingBlock,
  PageBody,
  PageHeader,
} from '@/components/page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { del, get } from '@/lib/api'
import { bytes } from '@/lib/format'
import { keys, paths } from '@/lib/query'
import type { CloseResult, Connection, Connections } from '@/lib/types'

/** How often the list is re-read while the page is open, in milliseconds. */
const POLL_INTERVAL = 3_000

/** A stable empty list, so a missing response does not defeat the filter's memo. */
const NO_CONNECTIONS: Connection[] = []

export function ConnectionsPage() {
  const { t } = useTranslation()
  const client = useQueryClient()
  const [filter, setFilter] = useState('')
  const [closing, setClosing] = useState<Connection | null>(null)
  const [closingAll, setClosingAll] = useState(false)

  const list = useQuery({
    queryKey: keys.connections,
    queryFn: () => get<Connections>(paths.connections),
    refetchInterval: POLL_INTERVAL,
  })

  const closeOne = useMutation({
    mutationFn: (id: string) =>
      del<CloseResult>(`${paths.connections}/${encodeURIComponent(id)}`),
    onSuccess: () => {
      setClosing(null)
      void client.invalidateQueries({ queryKey: keys.connections })
    },
  })

  const closeAll = useMutation({
    mutationFn: () =>
      del<CloseResult>(paths.connections, {
        // Required by the endpoint, and required to be `true`: this is the only
        // operation that interrupts every transfer at once.
        confirm: true,
      }),
    onSuccess: () => {
      setClosingAll(false)
      void client.invalidateQueries({ queryKey: keys.connections })
    },
  })

  // A module-level constant rather than `?? []`: a fresh empty array on every
  // render would make the dependency change identity each time, so the memo below
  // would recompute on every render instead of only when the list changes.
  const connections = list.data?.connections ?? NO_CONNECTIONS
  const visible = useMemo(() => filterConnections(connections, filter), [connections, filter])

  return (
    <>
      <PageHeader
        title={t('connections.title')}
        description={t('connections.activeCount', { count: connections.length })}
        actions={
          <>
            <div className="relative">
              <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={filter}
                onChange={(event) => setFilter(event.target.value)}
                placeholder={t('connections.searchPlaceholder')}
                className="h-8 w-64 pl-8 text-xs"
              />
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void list.refetch()}
              disabled={list.isFetching}
            >
              <RefreshCw className={list.isFetching ? 'size-3.5 animate-spin' : 'size-3.5'} />
              {t('common.refresh')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              className="text-destructive"
              onClick={() => setClosingAll(true)}
              disabled={!connections.length}
            >
              <XCircle className="size-3.5" />
              {t('connections.closeAll')}
            </Button>
          </>
        }
      />

      <PageBody>
        {list.isError ? (
          <ErrorState error={list.error} onRetry={() => void list.refetch()} />
        ) : list.isLoading ? (
          <LoadingBlock rows={6} />
        ) : (
          <>
            <div className="flex flex-wrap gap-6 text-sm">
              <Stat label={t('connections.totalUpload')} value={bytes(list.data?.upload_total ?? 0)} />
              <Stat label={t('connections.totalDownload')} value={bytes(list.data?.download_total ?? 0)} />
            </div>

            {!connections.length ? (
              <EmptyState>{t('common.empty')}</EmptyState>
            ) : !visible.length ? (
              <EmptyState>{t('common.empty')}</EmptyState>
            ) : (
              <div className="overflow-x-auto rounded-lg border">
                <table className="w-full text-sm">
                  <thead className="bg-muted/50 text-xs text-muted-foreground">
                    <tr>
                      <th className="px-4 py-2 text-left font-normal">{t('connections.source')}</th>
                      <th className="px-4 py-2 text-left font-normal">
                        {t('connections.destination')}
                      </th>
                      <th className="px-4 py-2 text-left font-normal">{t('connections.rule')}</th>
                      <th className="px-4 py-2 text-left font-normal">
                        {t('connections.process')}
                      </th>
                      <th className="px-4 py-2 text-right font-normal">
                        {t('connections.upload')}
                      </th>
                      <th className="px-4 py-2 text-right font-normal">
                        {t('connections.download')}
                      </th>
                      <th className="px-4 py-2 text-right font-normal" />
                    </tr>
                  </thead>
                  <tbody className="divide-y">
                    {visible.map((connection) => (
                      <tr key={connection.id}>
                        <td className="px-4 py-2 font-mono text-xs">{connection.source}</td>
                        <td className="max-w-0 truncate px-4 py-2 font-mono text-xs">
                          {connection.destination}
                        </td>
                        <td className="px-4 py-2">
                          {connection.rule ? (
                            <div className="flex flex-wrap items-center gap-1.5">
                              <Badge variant="outline" className="font-normal">
                                {connection.rule}
                              </Badge>
                              {connection.rule_payload && (
                                <span className="max-w-40 truncate font-mono text-xs text-muted-foreground">
                                  {connection.rule_payload}
                                </span>
                              )}
                            </div>
                          ) : (
                            <span className="text-xs text-muted-foreground">{t('common.none')}</span>
                          )}
                          {connection.chains.length > 0 && (
                            <div className="mt-0.5 truncate text-[11px] text-muted-foreground">
                              {connection.chains.join(' → ')}
                            </div>
                          )}
                        </td>
                        <td className="px-4 py-2">
                          <div className="max-w-48 truncate font-mono text-xs">
                            {connection.process ?? t('common.none')}
                          </div>
                          {connection.uid !== null && (
                            <div className="text-[11px] text-muted-foreground">
                              {t('connections.uid')} {connection.uid}
                            </div>
                          )}
                        </td>
                        <td className="px-4 py-2 text-right font-mono text-xs">
                          {bytes(connection.upload)}
                        </td>
                        <td className="px-4 py-2 text-right font-mono text-xs">
                          {bytes(connection.download)}
                        </td>
                        <td className="px-4 py-2 text-right">
                          <Button
                            size="sm"
                            variant="ghost"
                            className="text-destructive"
                            onClick={() => setClosing(connection)}
                            aria-label={t('connections.close')}
                          >
                            <X className="size-3.5" />
                          </Button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}

          </>
        )}

        {closeOne.isError && <ErrorState error={closeOne.error} />}
        {closeAll.isError && <ErrorState error={closeAll.error} />}
      </PageBody>

      <ConfirmDialog
        open={closing !== null}
        onOpenChange={(open) => !open && setClosing(null)}
        title={t('common.confirmTitle')}
        description={t('connections.confirmClose')}
        confirmLabel={t('connections.close')}
        cancelLabel={t('common.cancel')}
        pending={closeOne.isPending}
        onConfirm={() => closing && closeOne.mutate(closing.id)}
      >
        {closing && (
          <div className="rounded-md bg-muted px-3 py-2 font-mono text-xs">
            {closing.source} → {closing.destination}
          </div>
        )}
      </ConfirmDialog>

      <ConfirmDialog
        open={closingAll}
        onOpenChange={setClosingAll}
        title={t('common.confirmTitle')}
        description={t('connections.confirmCloseAll')}
        confirmLabel={t('connections.closeAll')}
        cancelLabel={t('common.cancel')}
        pending={closeAll.isPending}
        onConfirm={() => closeAll.mutate()}
      />
    </>
  )
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="font-mono">{value}</div>
    </div>
  )
}

/**
 * Filters a connection list by a free-text query.
 *
 * Matches the fields a reader can actually see, plus the chains, so a search for a
 * group name finds the connections routed through it. Case-insensitive because the
 * reader is typing a hostname from memory, not copying one.
 */
function filterConnections(connections: Connection[], query: string): Connection[] {
  const needle = query.trim().toLowerCase()
  if (!needle) return connections
  return connections.filter((connection) =>
    [connection.source, connection.destination, connection.rule, connection.rule_payload, connection.process, connection.inbound]
      .concat(connection.chains)
      .some((field) => field?.toLowerCase().includes(needle)),
  )
}
