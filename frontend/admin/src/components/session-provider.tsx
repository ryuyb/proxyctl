/**
 * The session provider: one `GET /session` for the whole interface.
 *
 * # Why the session is state rather than a per-request concern
 *
 * Every page needs to know two things — whether anyone is signed in, and what they
 * may do — and both answers come from the same request. Letting each page ask
 * would mean N identical requests on a dashboard whose panels load in parallel,
 * and would let two panels disagree about the role while both are rendering.
 *
 * # Why a `401` anywhere signs the caller out
 *
 * A session expires on the agent's schedule, not on this page's. When it does,
 * the next request answers `401`, and the honest response is the sign-in page
 * rather than a panel of error messages. That is what `onUnauthenticated` is for;
 * it is wired into the query client in `main.tsx`.
 */

import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react'

import { SessionContext, fetchSession, signIn as postSignIn, signOut as postSignOut, type SessionState } from '@/lib/session'
import type { Session } from '@/lib/types'

/** Remembers that the agent asked us to sign in again. */
const EXPIRED_KEY = 'proxyctl.sessionExpired'

export function SessionProvider({ children }: { children: ReactNode }) {
  const [session, setSession] = useState<Session | null>(null)
  const [loading, setLoading] = useState(true)

  const refresh = useCallback(async () => {
    try {
      setSession(await fetchSession())
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void refresh()
  }, [refresh])

  const signIn = useCallback(async (token: string) => {
    const signedIn = await postSignIn(token)
    clearExpiredFlag()
    setSession(signedIn)
  }, [])

  const signOut = useCallback(async () => {
    // The local state is cleared even if the request fails. The caller's intent is
    // that this browser no longer be signed in, and a network failure must not
    // leave the interface looking signed in.
    try {
      await postSignOut()
    } finally {
      clearExpiredFlag()
      setSession(null)
    }
  }, [])

  const value = useMemo<SessionState>(
    () => ({ session, loading, signIn, signOut, refresh }),
    [session, loading, signIn, signOut, refresh],
  )

  return <SessionContext.Provider value={value}>{children}</SessionContext.Provider>
}

/**
 * Marks the session as expired by the agent rather than never established.
 *
 * The distinction is worth a persistent flag because it changes what the sign-in
 * page should say: "your session ended" is actionable, and "sign in" after a page
 * that was working a moment ago reads like a bug.
 */
export function markExpired(): void {
  try {
    sessionStorage.setItem(EXPIRED_KEY, '1')
  } catch {
    // See `readStored` in `lib/i18n.ts`. Losing the nuance is acceptable.
  }
}

/** Consumes the expired flag, returning whether it had been set. */
export function clearExpiredFlag(): boolean {
  try {
    const was = sessionStorage.getItem(EXPIRED_KEY) === '1'
    sessionStorage.removeItem(EXPIRED_KEY)
    return was
  } catch {
    return false
  }
}

/** Reads the expired flag without clearing it. */
export function wasExpired(): boolean {
  try {
    return sessionStorage.getItem(EXPIRED_KEY) === '1'
  } catch {
    return false
  }
}
