/**
 * The sign-in page.
 *
 * # Why the token is never held after this
 *
 * It is posted once and immediately exchanged for an `HttpOnly` cookie. Nothing
 * keeps it — not state, not storage — so a script that later gets onto this page
 * has nothing to steal. The `privacy` string states this to the reader, because an
 * operator pasting a bearer credential into a web form deserves to be told where
 * it goes.
 */

import { useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'
import { KeyRound } from 'lucide-react'

import { LanguageToggle } from '@/components/language-toggle'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { ApiError } from '@/lib/api'
import { useSession } from '@/lib/session'
import { wasExpired } from '@/components/session-provider'

export function SignInPage() {
  const { t } = useTranslation()
  const { signIn } = useSession()
  const [token, setToken] = useState('')
  const [busy, setBusy] = useState(false)
  const [failure, setFailure] = useState<string | null>(null)
  // Read once, at mount: the flag is cleared by a successful sign-in, and reading
  // it per render would make the message disappear mid-attempt.
  const [expired] = useState(() => wasExpired())

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    if (!token.trim() || busy) return
    setBusy(true)
    setFailure(null)
    try {
      await signIn(token)
      // The token is dropped from state before anything else can render it.
      setToken('')
    } catch (cause) {
      if (cause instanceof ApiError) {
        // The agent's own message is used when it sent one. It is a refusal, not
        // a diagnostic, so it is safe to show and more specific than anything
        // this page could invent.
        setFailure(cause.isUnauthenticated ? t('signIn.failed') : cause.detail)
      } else {
        setFailure(cause instanceof Error ? cause.message : String(cause))
      }
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center bg-muted/30 p-4">
      <div className="w-full max-w-sm space-y-4">
        <div className="flex justify-end">
          <LanguageToggle />
        </div>

        <Card>
          <CardHeader>
            <div className="mb-2 flex size-9 items-center justify-center rounded-lg bg-primary text-primary-foreground">
              <KeyRound className="size-4" />
            </div>
            <CardTitle>{t('signIn.title')}</CardTitle>
            <CardDescription>
              {expired ? t('common.forbidden') : t('signIn.subtitle')}
            </CardDescription>
          </CardHeader>

          <CardContent>
            <form onSubmit={(event) => void submit(event)} className="space-y-4">
              <div className="space-y-2">
                <Label htmlFor="token">{t('signIn.tokenLabel')}</Label>
                <Input
                  id="token"
                  type="password"
                  value={token}
                  onChange={(event) => setToken(event.target.value)}
                  placeholder={t('signIn.tokenPlaceholder')}
                  autoComplete="off"
                  autoFocus
                  // A token is pasted, not typed. Neither the browser's password
                  // manager nor its autofill should record it.
                  name="proxyctl-token"
                  spellCheck={false}
                  disabled={busy}
                />
              </div>

              {failure && (
                <Alert variant="destructive">
                  <AlertTitle>{t('signIn.failed')}</AlertTitle>
                  <AlertDescription>{failure}</AlertDescription>
                </Alert>
              )}

              <Button type="submit" className="w-full" disabled={busy || !token.trim()}>
                {busy ? t('signIn.submitting') : t('signIn.submit')}
              </Button>
            </form>

            <div className="mt-4 space-y-1 text-xs text-muted-foreground">
              <p>{t('signIn.privacy')}</p>
              <p>
                <code className="rounded bg-muted px-1 py-0.5">{t('signIn.hint')}</code>
              </p>
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  )
}
