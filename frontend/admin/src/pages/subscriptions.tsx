/**
 * Subscriptions: their schedule, their last outcome, and updating them by hand.
 *
 * # Why the URL is masked after creation
 *
 * A subscription URL usually carries a token. The agent stores it and never
 * returns it — the list endpoint does not serialise it — so this page has nothing
 * to mask; it renders what the agent sends, which is a name and an outcome. The
 * form's own hint says so, because an operator pasting a credential deserves to
 * know it is not coming back.
 *
 * # Why `update now` does not confirm
 *
 * An update cannot destroy anything: the agent validates the conversion and keeps
 * the previous configuration active when anything fails. A confirmation would
 * imply a risk that the architecture exists to remove.
 */

import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertCircle, CheckCircle2, Plus, RefreshCw, Trash2, Upload } from 'lucide-react'

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
import { Label } from '@/components/ui/label'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { get, post, del, patch } from '@/lib/api'
import { duration } from '@/lib/format'
import { keys, paths } from '@/lib/query'
import type { Job, Subscription, SubscriptionInput } from '@/lib/types'

/** The schedules a subscription may be given. */
const SCHEDULES = [0, 3600, 21600, 43200, 86400] as const

export function SubscriptionsPage() {
  const { t } = useTranslation()
  const client = useQueryClient()
  const [editing, setEditing] = useState<Subscription | 'new' | null>(null)
  const [removing, setRemoving] = useState<Subscription | null>(null)

  const list = useQuery({
    queryKey: keys.subscriptions,
    queryFn: () => get<Subscription[]>(paths.subscriptions),
  })

  const invalidate = () => {
    void client.invalidateQueries({ queryKey: keys.subscriptions })
    void client.invalidateQueries({ queryKey: keys.jobs })
  }

  const updateNow = useMutation({
    mutationFn: (id: string) => post<Job>(`${paths.subscriptions}/${encodeURIComponent(id)}/update`),
    onSuccess: invalidate,
  })

  const remove = useMutation({
    mutationFn: (id: string) => del<void>(`${paths.subscriptions}/${encodeURIComponent(id)}`),
    onSuccess: () => {
      setRemoving(null)
      invalidate()
    },
  })

  return (
    <>
      <PageHeader
        title={t('subscriptions.title')}
        actions={
          <>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void list.refetch()}
              disabled={list.isFetching}
            >
              <RefreshCw className={list.isFetching ? 'size-3.5 animate-spin' : 'size-3.5'} />
              {t('common.refresh')}
            </Button>
            <Button size="sm" onClick={() => setEditing('new')}>
              <Plus className="size-3.5" />
              {t('subscriptions.add')}
            </Button>
          </>
        }
      />

      <PageBody>
        {list.isError ? (
          <ErrorState error={list.error} onRetry={() => void list.refetch()} />
        ) : list.isLoading ? (
          <LoadingBlock rows={4} />
        ) : !list.data?.length ? (
          <EmptyState>{t('subscriptions.add')}</EmptyState>
        ) : (
          <div className="space-y-2">
            {list.data.map((subscription) => (
              <SubscriptionRow
                key={subscription.id}
                subscription={subscription}
                busy={updateNow.isPending && updateNow.variables === subscription.id}
                onUpdate={() => updateNow.mutate(subscription.id)}
                onEdit={() => setEditing(subscription)}
                onRemove={() => setRemoving(subscription)}
              />
            ))}
          </div>
        )}

        {updateNow.isError && <ErrorState error={updateNow.error} />}
        {remove.isError && <ErrorState error={remove.error} />}
      </PageBody>

      {editing && (
        <SubscriptionDialog
          subscription={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null)
            invalidate()
          }}
        />
      )}

      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t('common.confirmTitle')}
        description={t('subscriptions.confirmRemove')}
        confirmLabel={t('subscriptions.remove')}
        cancelLabel={t('common.cancel')}
        pending={remove.isPending}
        onConfirm={() => removing && remove.mutate(removing.id)}
      />
    </>
  )
}

function SubscriptionRow({
  subscription,
  busy,
  onUpdate,
  onEdit,
  onRemove,
}: {
  subscription: Subscription
  busy: boolean
  onUpdate: () => void
  onEdit: () => void
  onRemove: () => void
}) {
  const { t } = useTranslation()
  const succeeded = subscription.last_update === 'succeeded'

  return (
    <div className="flex flex-wrap items-center justify-between gap-4 rounded-lg border px-4 py-3">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate font-medium">{subscription.name}</span>
          {subscription.is_due && (
            <Badge variant="secondary" className="shrink-0 text-[10px]">
              {t('subscriptions.due')}
            </Badge>
          )}
          {!subscription.enabled && (
            <Badge variant="outline" className="shrink-0 text-[10px]">
              {t('common.no')}
            </Badge>
          )}
        </div>
        <div className="mt-1 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
          <span className="font-mono">{subscription.id}</span>
          <span>
            {subscription.interval_seconds == null
              ? t('subscriptions.intervalNone')
              : t('subscriptions.intervalValue', { seconds: subscription.interval_seconds })}
          </span>
          {subscription.last_update && (
            <span className="inline-flex items-center gap-1">
              {succeeded ? (
                <CheckCircle2 className="size-3 text-status-ok" />
              ) : (
                <AlertCircle className="size-3 text-status-error" />
              )}
              {/* The agent's outcome label is translated when it is one this
               * bundle knows, and shown raw otherwise. */}
              {t(`status.${subscription.last_update}`, {
                defaultValue: subscription.last_update,
              })}
            </span>
          )}
        </div>
      </div>

      <div className="flex shrink-0 items-center gap-1">
        <Button size="sm" variant="ghost" onClick={onUpdate} disabled={busy}>
          <Upload className={busy ? 'size-3.5 animate-pulse' : 'size-3.5'} />
          {t('subscriptions.updateNow')}
        </Button>
        <Button size="sm" variant="ghost" onClick={onEdit}>
          {t('subscriptions.edit')}
        </Button>
        <Button
          size="sm"
          variant="ghost"
          className="text-destructive"
          onClick={onRemove}
          aria-label={t('subscriptions.remove')}
        >
          <Trash2 className="size-3.5" />
        </Button>
      </div>
    </div>
  )
}

/**
 * The create and edit form.
 *
 * One component for both, because the fields are identical and the only difference
 * is whether saving is a `POST` or a `PATCH`. Two components would drift.
 */
function SubscriptionDialog({
  subscription,
  onClose,
  onSaved,
}: {
  subscription: Subscription | null
  onClose: () => void
  onSaved: () => void
}) {
  const { t } = useTranslation()
  const [name, setName] = useState(subscription?.name ?? '')
  // A URL is never returned by the agent, so on edit this starts empty and an
  // empty value means "leave it alone" rather than "clear it". That is stated in
  // the hint rather than left to be inferred from a save that did nothing.
  const [url, setUrl] = useState('')
  const [userAgent, setUserAgent] = useState('')
  const [schedule, setSchedule] = useState(String(subscription?.interval_seconds ?? 0))

  const save = useMutation({
    mutationFn: async () => {
      const seconds = Number(schedule)
      if (subscription) {
        const body: Partial<SubscriptionInput> = { name }
        if (url.trim()) body.url = url.trim()
        if (userAgent.trim()) body.user_agent = userAgent.trim()
        body.schedule_seconds = seconds === 0 ? null : seconds
        return patch<{ id: string }>(
          `${paths.subscriptions}/${encodeURIComponent(subscription.id)}`,
          body,
        )
      }
      return post<{ id: string }>(paths.subscriptions, {
        name,
        url,
        user_agent: userAgent.trim() || null,
        schedule_seconds: seconds === 0 ? null : seconds,
      })
    },
    onSuccess: onSaved,
  })

  const valid = name.trim() !== '' && (subscription !== null || url.trim() !== '')

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {subscription ? t('subscriptions.edit') : t('subscriptions.addTitle')}
          </DialogTitle>
          <DialogDescription>{t('subscriptions.urlHint')}</DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="name">{t('subscriptions.name')}</Label>
            <Input
              id="name"
              value={name}
              onChange={(event) => setName(event.target.value)}
              autoFocus
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="url">{t('subscriptions.url')}</Label>
            <Input
              id="url"
              value={url}
              onChange={(event) => setUrl(event.target.value)}
              placeholder={t('subscriptions.urlPlaceholder')}
              spellCheck={false}
              autoComplete="off"
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="user-agent">{t('subscriptions.userAgent')}</Label>
            <Input
              id="user-agent"
              value={userAgent}
              onChange={(event) => setUserAgent(event.target.value)}
              spellCheck={false}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="schedule">{t('subscriptions.schedule')}</Label>
            <Select value={schedule} onValueChange={setSchedule}>
              <SelectTrigger id="schedule">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {SCHEDULES.map((seconds) => (
                  <SelectItem key={seconds} value={String(seconds)}>
                    {seconds === 0
                      ? t('subscriptions.scheduleNone')
                      : duration(seconds, t)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          {save.isError && <ErrorState error={save.error} />}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={save.isPending}>
            {t('common.cancel')}
          </Button>
          <Button onClick={() => save.mutate()} disabled={save.isPending || !valid}>
            {t('common.confirm')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
