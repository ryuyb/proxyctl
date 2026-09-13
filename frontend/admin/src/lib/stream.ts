/**
 * Reading a newline-delimited JSON stream from `fetch`.
 *
 * # What this has to get right
 *
 * `/ws/v1/events` and `/logs` are chunked bodies of JSON values separated by
 * newlines — not WebSockets, and not one JSON array. Reading one with `res.json()`
 * or `res.text()` would wait for the body to end, which for these endpoints is
 * never. So the reading has to happen incrementally, and two details make that
 * harder than it looks:
 *
 * 1. **A chunk boundary is not a line boundary.** One `read()` can return half a
 *    line, and the next returns the rest. A reader that decodes each chunk
 *    independently will fail on any line the network happened to split, which is
 *    rare enough to pass a test and frequent enough to fail in production.
 * 2. **A multi-byte character can be split.** UTF-8 characters outside ASCII are
 *    one to four bytes, and a boundary can fall inside one. `TextDecoder` is used
 *    with `{ stream: true }`, which holds an incomplete sequence until the rest
 *    arrives; decoding each chunk with a fresh decoder would corrupt it.
 *
 * # Cancellation
 *
 * The caller passes an `AbortSignal`. Every stream here is endless by design, so
 * the only way to stop one is to abort it, and a component that unmounts without
 * aborting leaves a reader attached to a socket.
 */

/** One parsed line, with the raw text kept for diagnostics. */
export interface Line<T> {
  value: T
  raw: string
}

/** Options for {@link readLines}. */
export interface ReadLinesOptions<T> {
  /** Stops the stream. Required, because these streams never end on their own. */
  signal: AbortSignal
  /** Called for each parsed line, in order. */
  onLine: (line: Line<T>) => void
  /**
   * Called for a line that is not valid JSON.
   *
   * Optional, and deliberately separate from `onLine`: a malformed line is not
   * something to render as data, but silently dropping it would hide a real
   * problem. The default is to ignore it.
   */
  onMalformed?: (raw: string) => void
  /** Called when the body ends, whether cleanly or because it was aborted. */
  onEnd?: (reason: 'closed' | 'aborted' | 'error', error?: unknown) => void
}

/**
 * Reads an NDJSON body until it ends or the signal aborts.
 *
 * Resolves when the body has been fully consumed, or when the signal aborts.
 * Never rejects for a body that ends: an endpoint closing its stream is the normal
 * way these endpoints stop.
 */
export async function readLines<T>(path: string, options: ReadLinesOptions<T>): Promise<void> {
  const { signal, onLine, onMalformed, onEnd } = options

  let response: Response
  try {
    response = await fetch(path, {
      method: 'GET',
      headers: { accept: 'application/x-ndjson' },
      signal,
    })
  } catch (cause) {
    if (signal.aborted) {
      onEnd?.('aborted')
      return
    }
    onEnd?.('error', cause)
    return
  }

  if (!response.ok) {
    onEnd?.('error', new Error(`the stream answered ${response.status}`))
    return
  }
  if (!response.body) {
    onEnd?.('error', new Error('the response has no body to read'))
    return
  }

  const reader = response.body.getReader()
  // `stream: true` is what makes a multi-byte character split across two chunks
  // survive: without it, each chunk is decoded as if it were complete.
  const decoder = new TextDecoder()
  let buffered = ''

  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break

      buffered += decoder.decode(value, { stream: true })

      // A trailing partial line is kept in `buffered` until its newline arrives.
      let newline = buffered.indexOf('\n')
      while (newline >= 0) {
        const raw = buffered.slice(0, newline)
        buffered = buffered.slice(newline + 1)
        emit(raw, onLine, onMalformed)
        newline = buffered.indexOf('\n')
      }
    }

    // The body ended. Anything left without a trailing newline is still a
    // complete value — a server that closed right after writing one would
    // otherwise lose it.
    buffered += decoder.decode()
    if (buffered.trim()) emit(buffered, onLine, onMalformed)

    onEnd?.(signal.aborted ? 'aborted' : 'closed')
  } catch (cause) {
    onEnd?.(signal.aborted ? 'aborted' : 'error', cause)
  } finally {
    // Releasing the lock lets the body be cancelled; without it, aborting the
    // signal leaves the reader held.
    reader.releaseLock()
  }
}

/** Parses and dispatches one line. */
function emit<T>(
  raw: string,
  onLine: (line: Line<T>) => void,
  onMalformed?: (raw: string) => void,
): void {
  const text = raw.trim()
  if (!text) return
  try {
    onLine({ value: JSON.parse(text) as T, raw: text })
  } catch {
    onMalformed?.(text)
  }
}
