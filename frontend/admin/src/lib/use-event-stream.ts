/**
 * The event stream: one connection, shared by every page.
 *
 * # What it does with an event
 *
 * An event says *that* something changed, not what to display. The response is to
 * invalidate the queries that could have been affected and let them re-read. That
 * is deliberate: the event payloads are deliberately thin — a subscription event
 * carries a success flag rather than the outcome, a config event carries a version
 * identifier rather than the version — precisely so no payload is rich enough to
 * render from. Re-reading through the API means one path produces what is on
 * screen, so a panel cannot disagree with the list it came from.
 *
 * # Why it reconnects
 *
 * The agent restarts, the network drops, a proxy times out. None of those are
 * reasons for the interface to stop being live, and a stream that gave up would
 * leave a window that looks current but is not — the worst state for this tool.
 * The backoff is bounded: this is a local agent, and a long delay would leave the
 * operator staring at stale data over a blip that lasted a moment.
 */

import { useEffect, useRef, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'

import { keys, paths } from './query'
import { readLines } from './stream'
import type { AgentEvent } from './types'

/** How the stream is doing. */
export type StreamState = 'connecting' | 'live' | 'reconnecting'

/** The first reconnect delay, in milliseconds. */
const BASE_DELAY = 1_000

/** The longest reconnect delay. Capped so a blip recovers promptly. */
const MAX_DELAY = 15_000

/** Subscribes to the agent's event stream for the lifetime of the component. */
export function useEventStream(): StreamState {
  const client = useQueryClient()
  const [state, setState] = useState<StreamState>('connecting')

  // Kept in a ref so the stream's callbacks can read the current client without
  // the effect depending on it. Assigned in an effect rather than during render:
  // a render can be thrown away or replayed under concurrent rendering, so
  // mutating a ref there would leave the ref holding a value from a render that
  // never committed.
  const clientRef = useRef(client)
  useEffect(() => {
    clientRef.current = client
  }, [client])

  useEffect(() => {
    const controller = new AbortController()
    let delay = BASE_DELAY
    let stopped = false
    let timer: ReturnType<typeof setTimeout> | undefined

    const connect = async () => {
      if (stopped) return
      setState((previous) => (previous === 'live' ? 'reconnecting' : previous))

      await readLines<AgentEvent>(paths.events, {
        signal: controller.signal,
        onLine: ({ value }) => {
          // The first line proves the stream is up, whatever it says. A heartbeat
          // is the common case on a quiet system.
          setState('live')
          delay = BASE_DELAY
          if (value?.kind) invalidateFor(clientRef.current, value.kind)
        },
        onEnd: (reason) => {
          if (reason === 'aborted' || stopped) return
          setState('reconnecting')
          // Every reconnect in this stream is also a reason to re-read everything:
          // events were missed while the connection was down, and there is no
          // replay. Doing it here rather than on the first event after reconnect
          // means a quiet system still converges.
          clientRef.current.invalidateQueries()
          timer = setTimeout(() => {
            void connect()
          }, delay)
          // Bounded exponential backoff. The agent is local, so a long wait would
          // keep the operator looking at stale data over a blip that lasted a
          // moment.
          delay = Math.min(delay * 2, MAX_DELAY)
        },
      })
    }

    void connect()

    return () => {
      stopped = true
      if (timer) clearTimeout(timer)
      controller.abort()
    }
  }, [])

  return state
}

/**
 * Invalidates the queries an event could have changed.
 *
 * # Why this only invalidates, and never writes
 *
 * Writing the event's payload into the cache would mean the cache holds a shape
 * that only this function knows how to build, and the next real read would have to
 * agree with it. Invalidating lets the API stay the only producer of the data on
 * screen.
 *
 * `mihomo.log` is absent on purpose: a log line changes no query's answer.
 */
function invalidateFor(client: ReturnType<typeof useQueryClient>, kind: string): void {
  switch (kind) {
    case 'config.activated':
    case 'config.rolled_back':
      void client.invalidateQueries({ queryKey: keys.configs })
      void client.invalidateQueries({ queryKey: keys.mihomo })
      void client.invalidateQueries({ queryKey: keys.jobs })
      break
    case 'subscription.updated':
      void client.invalidateQueries({ queryKey: keys.subscriptions })
      void client.invalidateQueries({ queryKey: keys.jobs })
      break
    case 'job.progress':
    case 'job.finished':
      void client.invalidateQueries({ queryKey: keys.jobs })
      // A finished job may have restarted the kernel or activated a config, so the
      // status is re-read too. One extra request against a local agent is cheaper
      // than a page that is confidently wrong.
      void client.invalidateQueries({ queryKey: keys.mihomo })
      break
    case 'lagged':
      // Events were dropped, so nothing specific can be concluded. Everything is
      // re-read, which is the only sound response to "you missed some".
      void client.invalidateQueries()
      break
    default:
      break
  }
}
