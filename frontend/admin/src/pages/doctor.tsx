/**
 * The doctor: what the agent probed, and what it concluded.
 *
 * # Why findings include the checks that passed
 *
 * The agent reports clean checks alongside problems rather than only reporting
 * failures. An empty list would be ambiguous — it could mean "everything is fine"
 * or "the check never ran" — and the two call for completely different responses.
 * Rendering every finding removes that ambiguity, and the severity ordering below
 * puts the answers first.
 */

import { useTranslation } from 'react-i18next'
import { useQuery } from '@tanstack/react-query'
import { Stethoscope } from 'lucide-react'

import {
  EmptyState,
  ErrorState,
  LoadingBlock,
  PageBody,
  PageHeader,
  SeverityBadge,
} from '@/components/page'
import { Button } from '@/components/ui/button'
import { get } from '@/lib/api'
import { keys, paths } from '@/lib/query'
import type { Doctor, Finding } from '@/lib/types'

/**
 * The order findings are shown in.
 *
 * Most serious first, because the list is long and the reader's question is
 * "what is wrong". `pass` is last, so it is available as evidence without pushing
 * a problem below the fold.
 */
const SEVERITY_ORDER: Record<string, number> = { error: 0, warning: 1, info: 2, pass: 3 }

export function DoctorPage() {
  const { t } = useTranslation()

  const report = useQuery({
    queryKey: keys.doctor,
    queryFn: () => get<Doctor>(paths.doctor),
  })

  const findings = [...(report.data?.findings ?? [])].sort(
    (a, b) => (SEVERITY_ORDER[a.severity] ?? 9) - (SEVERITY_ORDER[b.severity] ?? 9),
  )

  return (
    <>
      <PageHeader
        title={t('doctor.title')}
        actions={
          <Button
            size="sm"
            onClick={() => void report.refetch()}
            disabled={report.isFetching}
          >
            <Stethoscope className={report.isFetching ? 'size-3.5 animate-pulse' : 'size-3.5'} />
            {report.isFetching ? t('doctor.running') : t('doctor.run')}
          </Button>
        }
      />

      <PageBody>
        {report.isError ? (
          <ErrorState error={report.error} onRetry={() => void report.refetch()} />
        ) : report.isLoading ? (
          <LoadingBlock rows={6} />
        ) : report.data ? (
          <>
            <div className="flex items-center gap-3 rounded-lg border px-4 py-3">
              <span className="text-xs text-muted-foreground">{t('doctor.verdict')}</span>
              <SeverityBadge severity={verdictSeverity(report.data.verdict, report.data.findings)} />
              <span className="font-mono text-sm">{report.data.verdict}</span>
            </div>

            {!findings.length ? (
              <EmptyState>{t('doctor.clean')}</EmptyState>
            ) : (
              <div className="space-y-2">
                {findings.map((finding, index) => (
                  <FindingRow key={`${finding.code}-${index}`} finding={finding} />
                ))}
              </div>
            )}

            <div className="grid grid-cols-2 gap-x-6 gap-y-3 rounded-lg border px-4 py-3 text-xs sm:grid-cols-3 lg:grid-cols-6">
              <Field label={t('system.os')} value={report.data.environment.os} />
              <Field label={t('system.osVersion')} value={report.data.environment.os_version} />
              <Field label={t('system.arch')} value={report.data.environment.arch} />
              <Field label={t('system.kernel')} value={report.data.environment.kernel} />
              <Field label={t('system.init')} value={report.data.environment.init} />
              <Field label={t('system.container')} value={report.data.environment.container} />
            </div>
          </>
        ) : null}
      </PageBody>
    </>
  )
}

function FindingRow({ finding }: { finding: Finding }) {
  const { t } = useTranslation()
  return (
    <div className="flex flex-wrap items-start gap-3 rounded-lg border px-4 py-3">
      <SeverityBadge severity={finding.severity} />
      <div className="min-w-0 flex-1">
        {/* The agent's own message verbatim. It names what it probed and what it
         * saw, which is the part this bundle cannot reconstruct. */}
        <p className="break-words text-sm">{finding.message}</p>
        <p className="mt-0.5 font-mono text-xs text-muted-foreground">
          {t('doctor.code')}: {finding.code}
        </p>
      </div>
    </div>
  )
}

/**
 * The severity to colour a verdict with.
 *
 * The verdict is the agent's own word and is shown as it arrived. Its colour is
 * derived from the findings, because the agent's vocabulary for a verdict is not
 * the same closed set as a finding's severity, and inventing a mapping for every
 * possible verdict string would mean this bundle silently colouring an unfamiliar
 * one as "fine".
 */
function verdictSeverity(verdict: string, findings: Finding[]): string {
  const worst = findings.reduce<string>((current, finding) => {
    const rank = SEVERITY_ORDER[finding.severity] ?? 9
    return rank < (SEVERITY_ORDER[current] ?? 9) ? finding.severity : current
  }, 'pass')
  // A verdict that names a problem is coloured by that, even when no finding
  // carries the same severity.
  if (/fail|error|unhealthy/i.test(verdict)) return 'error'
  if (/warn|degrad/i.test(verdict)) return 'warning'
  return worst
}

function Field({ label, value }: { label: string; value: string | null }) {
  return (
    <div className="min-w-0">
      <div className="text-muted-foreground">{label}</div>
      <div className="truncate font-mono" title={value ?? undefined}>
        {value ?? '—'}
      </div>
    </div>
  )
}
