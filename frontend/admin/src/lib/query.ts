/**
 * The query client, and the hooks the pages use to read the API.
 *
 * # Why every query is keyed here rather than at its call site
 *
 * A key written inline is a string that two call sites can spell differently, and
 * the symptom is a cache that never hits — silent, and only visible as extra
 * traffic. Centralising them makes a rename a compile error.
 *
 * # Why `retry` distinguishes failures
 *
 * A `401` will not succeed on a second attempt: the session is gone, and retrying
 * it three times just delays the sign-in page. A `4xx` in general is the agent
 * saying the request is wrong, which retrying cannot fix. Only a transport failure
 * or a `5xx` is worth another try.
 */

import { QueryClient } from '@tanstack/react-query'

import { ApiError } from './api'
import { API_PREFIX } from './api'

/** How long a successful read is considered fresh, in milliseconds. */
const STALE_TIME = 5_000

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: STALE_TIME,
      // A window that is not visible does not need to poll. The event stream is
      // what keeps a focused window current; this is the fallback for one that
      // has lost its stream.
      refetchIntervalInBackground: false,
      retry: (failureCount, error) => {
        if (error instanceof ApiError && error.status >= 400 && error.status < 500) {
          return false
        }
        return failureCount < 2
      },
    },
    mutations: {
      // A mutation has a side effect. Retrying one automatically risks performing
      // it twice — a second activation, a second close — and the operator is
      // better served by an error they can read.
      retry: false,
    },
  },
})

/** The query keys, in one place so two call sites cannot disagree. */
export const keys = {
  session: ['session'] as const,
  system: ['system'] as const,
  health: ['health'] as const,
  doctor: ['doctor'] as const,
  mihomo: ['mihomo'] as const,
  proxies: ['mihomo', 'proxies'] as const,
  kernel: ['mihomo', 'kernel'] as const,
  configs: ['configs'] as const,
  subscriptions: ['subscriptions'] as const,
  jobs: ['jobs'] as const,
  audit: ['audit'] as const,
  connections: ['connections'] as const,
}

/** The path for a keyed endpoint. */
export const paths = {
  system: `${API_PREFIX}/system`,
  health: `${API_PREFIX}/health`,
  doctor: `${API_PREFIX}/doctor`,
  mihomo: `${API_PREFIX}/mihomo`,
  proxies: `${API_PREFIX}/mihomo/proxies`,
  kernel: `${API_PREFIX}/mihomo/kernel`,
  configs: `${API_PREFIX}/configs`,
  subscriptions: `${API_PREFIX}/subscriptions`,
  jobs: `${API_PREFIX}/jobs`,
  audit: `${API_PREFIX}/audit`,
  connections: `${API_PREFIX}/connections`,
  logs: `${API_PREFIX}/logs`,
  events: '/ws/v1/events',
}
