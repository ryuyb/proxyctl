/**
 * Routing, and the gate in front of it.
 *
 * # Why the gate is here rather than per page
 *
 * Every page needs a session, and no page is reachable without one. Checking per
 * page would mean each page renders its own "not signed in" state — which is how
 * three panels end up showing three different errors for one expired session.
 * Checking once means the pages below can assume a caller exists.
 *
 * # Why the sign-in page is not a route
 *
 * It is what an unauthenticated caller *sees*, not somewhere they navigate to.
 * Making it a route would mean it had a URL a signed-in caller could visit, and
 * then it would need to redirect, and the redirect would need to know where they
 * came from. Rendering it in place of the application avoids all three.
 */

import { useTranslation } from 'react-i18next'
import { Navigate, Route, Routes } from 'react-router-dom'

import { Layout } from '@/components/layout'
import { SignInPage } from '@/pages/sign-in'
import { ConfigsPage } from '@/pages/configs'
import { ConnectionsPage } from '@/pages/connections'
import { DoctorPage } from '@/pages/doctor'
import { LogsPage } from '@/pages/logs'
import { MihomoPage } from '@/pages/mihomo'
import { OverviewPage } from '@/pages/overview'
import { SubscriptionsPage } from '@/pages/subscriptions'
import { SystemPage } from '@/pages/system'
import { useSession } from '@/lib/session'

export function App() {
  const { session, loading } = useSession()
  const { t } = useTranslation()

  // Held rather than rendered as a spinner: a spinner that appears for one frame
  // on a fast connection reads as a flicker, and the alternative — rendering the
  // sign-in page before the answer arrives — signs a signed-in caller out
  // visually for the same frame.
  if (loading) {
    return (
      <div className="flex min-h-screen items-center justify-center text-muted-foreground">
        {t('common.loading')}
      </div>
    )
  }

  if (!session) {
    return <SignInPage />
  }

  return (
    <Routes>
      <Route element={<Layout />}>
        <Route index element={<OverviewPage />} />
        <Route path="mihomo" element={<MihomoPage />} />
        <Route path="configs" element={<ConfigsPage />} />
        <Route path="subscriptions" element={<SubscriptionsPage />} />
        <Route path="connections" element={<ConnectionsPage />} />
        <Route path="logs" element={<LogsPage />} />
        <Route path="system" element={<SystemPage />} />
        <Route path="doctor" element={<DoctorPage />} />
        {/* An unknown path returns to the overview rather than rendering nothing.
         * The agent serves the entry point for any unknown client-side route, so
         * a mistyped URL arrives here rather than at a 404, and an empty screen
         * would look like a failure to load. */}
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  )
}
