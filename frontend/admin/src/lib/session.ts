/**
 * The session: who is signed in.
 *
 * # Why this holds no credential
 *
 * Signing in posts a token once, and the agent answers with an `HttpOnly` cookie
 * the browser stores and attaches on its own. Nothing in this module — or anywhere
 * in the bundle — can read that cookie, which is the entire reason the session
 * design exists. If this file held a token in memory or in `localStorage`, any
 * script that got onto the page could take it.
 *
 * # Why there is no role
 *
 * Every signed-in caller may do everything this interface offers. There is no
 * `useIsAdmin` and no control that depends on one, because a control drawn for
 * some callers and not others would describe a boundary that does not exist: the
 * agent applies the same rules to every request.
 */

import { createContext, useContext } from 'react'

import { ApiError, API_PREFIX, del, get, post } from './api'
import type { Session } from './types'

/** What a component may rely on about the session. */
export interface SessionState {
  /** The signed-in principal, or `null` while unknown or signed out. */
  session: Session | null
  /** Whether the first `GET /session` is still in flight. */
  loading: boolean
  /** Signs in with a token, resolving when the cookie has been set. */
  signIn: (token: string) => Promise<void>
  /** Signs out and clears the cookie. */
  signOut: () => Promise<void>
  /** Re-reads the session, for after a `401` anywhere. */
  refresh: () => Promise<void>
}

/** The context, left `null` so a missing provider fails loudly. */
export const SessionContext = createContext<SessionState | null>(null)

/** Reads the session context, refusing to guess at a default. */
export function useSession(): SessionState {
  const value = useContext(SessionContext)
  if (!value) {
    throw new Error('useSession must be used inside a SessionProvider')
  }
  return value
}

/** Reads the current session, or `null` when not signed in. */
export async function fetchSession(): Promise<Session | null> {
  // `GET /session` answers 401 when there is no valid session. That is the
  // expected answer for a signed-out browser rather than a failure, so it is
  // converted to `null` here instead of surfacing as an error the caller must
  // special-case at every call site.
  try {
    return await get<Session>(`${API_PREFIX}/session`)
  } catch (cause) {
    if (cause instanceof ApiError && cause.isUnauthenticated) return null
    throw cause
  }
}

/** Exchanges a token for a session cookie. */
export async function signIn(token: string): Promise<Session> {
  return post<Session>(`${API_PREFIX}/session`, { token })
}

/** Revokes the session and clears the cookie. */
export async function signOut(): Promise<void> {
  await del<void>(`${API_PREFIX}/session`)
}
