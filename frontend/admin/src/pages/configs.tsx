/**
 * Configuration versions: what exists, what is active, and how to change it.
 *
 * # Why activation and rollback are separate verbs
 *
 * They are the same operation on the agent — both make a version active and reload
 * the kernel — but they are different intentions. An operator rolling back after a
 * bad activation is looking for the version that worked, and a column of identical
 * "Activate" buttons makes that search a memory test. The active row is marked, the
 * others are labelled by what they are: activating a version that was never active
 * is a rollback's inverse, but rolling back to one that was is a rollback.
 *
 * # Why validation is a separate panel
 *
 * Validating a document is the only thing here that takes input, and it is how an
 * operator tests a configuration before it becomes a version. Putting it behind a
 * dialog would make the one non-destructive operation on this page the hardest to
 * reach.
 */

import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CheckCircle2, FileCheck2, RefreshCw, Undo2, Zap } from 'lucide-react'

import { ConfirmDialog } from '@/components/confirm-dialog'
import {
  EmptyState,
  ErrorState,
  LoadingBlock,
  PageBody,
  PageHeader,
  Section,
} from '@/components/page'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Textarea } from '@/components/ui/textarea'
import { get, post } from '@/lib/api'
import { relative, shortChecksum, timestamp } from '@/lib/format'
import { keys, paths } from '@/lib/query'
import { useLanguage } from '@/lib/use-language'
import type { ConfigVersion, Job, Validation } from '@/lib/types'

export function ConfigsPage() {
  const { t } = useTranslation()
  const language = useLanguage()
  const client = useQueryClient()
  const [pending, setPending] = useState<{ action: 'activate' | 'rollback'; version: ConfigVersion } | null>(null)

  const configs = useQuery({
    queryKey: keys.configs,
    queryFn: () => get<ConfigVersion[]>(paths.configs),
  })

  const act = useMutation({
    mutationFn: ({ action, id }: { action: 'activate' | 'rollback'; id: string }) =>
      post<Job>(`${paths.configs}/${encodeURIComponent(id)}/${action}`),
    onSuccess: () => {
      setPending(null)
      void client.invalidateQueries({ queryKey: keys.configs })
      void client.invalidateQueries({ queryKey: keys.mihomo })
    },
  })

  return (
    <>
      <PageHeader
        title={t('configs.title')}
        actions={
          <Button
            variant="outline"
            size="sm"
            onClick={() => void configs.refetch()}
            disabled={configs.isFetching}
          >
            <RefreshCw className={configs.isFetching ? 'size-3.5 animate-spin' : 'size-3.5'} />
            {t('common.refresh')}
          </Button>
        }
      />

      <PageBody>
        {configs.isError ? (
          <ErrorState error={configs.error} onRetry={() => void configs.refetch()} />
        ) : configs.isLoading ? (
          <LoadingBlock rows={5} />
        ) : !configs.data?.length ? (
          <EmptyState>{t('common.empty')}</EmptyState>
        ) : (
          <div className="overflow-hidden rounded-lg border">
            <table className="w-full text-sm">
              <thead className="bg-muted/50 text-xs text-muted-foreground">
                <tr>
                  <th className="px-4 py-2 text-left font-normal">{t('configs.label')}</th>
                  <th className="px-4 py-2 text-left font-normal">{t('configs.source')}</th>
                  <th className="px-4 py-2 text-left font-normal">{t('configs.checksum')}</th>
                  <th className="px-4 py-2 text-left font-normal">{t('configs.createdAt')}</th>
                  <th className="px-4 py-2 text-right font-normal" />
                </tr>
              </thead>
              <tbody className="divide-y">
                {configs.data.map((version) => (
                  <tr key={version.id} className={version.active ? 'bg-accent/40' : undefined}>
                    <td className="px-4 py-2.5">
                      <div className="flex items-center gap-2">
                        <span className="font-mono font-medium">{version.label}</span>
                        {version.active && (
                          <Badge variant="secondary" className="gap-1 text-[10px]">
                            <CheckCircle2 className="size-3" />
                            {t('configs.active')}
                          </Badge>
                        )}
                      </div>
                    </td>
                    <td className="px-4 py-2.5 text-muted-foreground">{version.source}</td>
                    <td
                      className="px-4 py-2.5 font-mono text-xs text-muted-foreground"
                      // The full digest is what an operator compares against a
                      // file, so it stays reachable without widening the column.
                      title={version.checksum}
                    >
                      {shortChecksum(version.checksum)}
                    </td>
                    <td
                      className="px-4 py-2.5 text-muted-foreground"
                      title={timestamp(version.created_at, language)}
                    >
                      {relative(version.created_at, t)}
                    </td>
                    <td className="px-4 py-2.5 text-right">
                      {version.active ? (
                        <span className="text-xs text-muted-foreground">{t('configs.active')}</span>
                      ) : (
                        <div className="flex justify-end gap-1">
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => setPending({ action: 'activate', version })}
                          >
                            <Zap className="size-3.5" />
                            {t('configs.activate')}
                          </Button>
                          {version.activated_at !== null && (
                            <Button
                              size="sm"
                              variant="ghost"
                              onClick={() => setPending({ action: 'rollback', version })}
                            >
                              <Undo2 className="size-3.5" />
                              {t('configs.rollback')}
                            </Button>
                          )}
                        </div>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {act.isError && <ErrorState error={act.error} />}

        <ValidatePanel />
      </PageBody>

      <ConfirmDialog
        open={pending !== null}
        onOpenChange={(open) => !open && setPending(null)}
        title={t('common.confirmTitle')}
        description={
          pending?.action === 'rollback'
            ? t('configs.confirmRollback', { label: pending.version.label })
            : t('configs.confirmActivate')
        }
        confirmLabel={pending?.action === 'rollback' ? t('configs.rollback') : t('configs.activate')}
        cancelLabel={t('common.cancel')}
        pending={act.isPending}
        onConfirm={() =>
          pending && act.mutate({ action: pending.action, id: pending.version.id })
        }
      />
    </>
  )
}

/**
 * The validation panel.
 *
 * The three layers are reported separately rather than as one verdict, because
 * "the YAML parses" and "the configuration means something" are different answers
 * and a reader debugging a rejection needs to know which one failed.
 */
function ValidatePanel() {
  const { t } = useTranslation()
  const [body, setBody] = useState('')

  const validate = useMutation({
    mutationFn: () => post<Validation>(`${paths.configs}/validate`, { body }),
  })

  return (
    <Section title={t('configs.validateTitle')}>
      <Card>
        <CardContent className="space-y-3 pt-6">
          <Textarea
            value={body}
            onChange={(event) => setBody(event.target.value)}
            placeholder={t('configs.validatePlaceholder')}
            spellCheck={false}
            className="min-h-40 font-mono text-xs"
          />
          <div className="flex items-center gap-3">
            <Button
              size="sm"
              variant="outline"
              onClick={() => validate.mutate()}
              disabled={validate.isPending || !body.trim()}
            >
              <FileCheck2 className="size-3.5" />
              {t('configs.validate')}
            </Button>
            {validate.data && (
              <div className="flex flex-wrap items-center gap-3 text-xs">
                <Layer label={t('configs.preflight')} value={validate.data.preflight} />
                <Layer label={t('configs.syntax')} value={validate.data.syntax} />
                <Layer label={t('configs.semantic')} value={validate.data.semantic} />
                <Badge variant={validate.data.acceptable ? 'secondary' : 'destructive'}>
                  {t('configs.acceptable')}: {validate.data.acceptable ? t('common.yes') : t('common.no')}
                </Badge>
              </div>
            )}
          </div>
          {validate.isError && <ErrorState error={validate.error} />}
        </CardContent>
      </Card>
    </Section>
  )
}

/** One validation layer's outcome. */
function Layer({ label, value }: { label: string; value: string }) {
  return (
    <span className="text-muted-foreground">
      {label}: <span className="font-mono text-foreground">{value}</span>
    </span>
  )
}
