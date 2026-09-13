/**
 * The HTTP client for the agent's API.
 *
 * # Why this is hand-written rather than generated
 *
 * The response shapes are written out in `crates/interfaces/src/dto/mod.rs` and
 * mapped field by field from the application's views, precisely so the wire
 * format is a reviewed decision rather than a consequence of a type change. A
 * generator would remove that checkpoint on both sides at once, which is the one
 * thing that layer exists to prevent.
 *
 * # Authentication
 *
 * Requests carry the session cookie, which the browser attaches itself — there is
 * nothing here that reads or holds a credential. That is the point of the session
 * design: a script running on this page cannot obtain one. `credentials: 'same-origin'`
 * is the default and is therefore never stated.
 *
 * # Errors
 *
 * Every failure becomes an `ApiError` carrying the status and the agent's own
 * message. A component decides what to render; it never has to guess at a status
 * code, and it never sees a bare `Error` whose message is "fetch failed".
 */

/** The API version prefix, matching `API_PREFIX` on the server. */
export const API_PREFIX = '/api/v1'

/** The event stream's path. NDJSON over chunked HTTP, not a WebSocket. */
export const EVENTS_PATH = '/ws/v1/events'

/** A failure from the agent, or from reaching it. */
export class ApiError extends Error {
  /** The HTTP status, or 0 when the request never completed. */
  readonly status: number
  /** The agent's stable machine-readable code, when it sent one. */
  readonly code: string
  /** The agent's human-readable message, when it sent one. */
  readonly detail: string

  constructor(status: number, code: string, detail: string) {
    super(`${code}: ${detail}`)
    this.name = 'ApiError'
    this.status = status
    this.code = code
    this.detail = detail
  }

  /** Whether the caller is not signed in, or the session has expired. */
  get isUnauthenticated() {
    return this.status === 401
  }

  /** Whether the caller is signed in but not permitted. */
  get isForbidden() {
    return this.status === 403
  }

  /** Whether the agent could not be reached at all. */
  get isUnreachable() {
    return this.status === 0
  }
}

/** The shape of the agent's error body. */
interface ErrorBody {
  code?: unknown
  message?: unknown
}

/**
 * Performs a request and decodes its JSON body.
 *
 * `204` and an empty body are decoded as `undefined` rather than parsed: several
 * endpoints answer with no content, and `JSON.parse('')` throws.
 */
async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let response: Response
  try {
    response = await fetch(path, {
      ...init,
      headers: {
        // Stated rather than inferred so a proxy cannot serve a cached API
        // response in place of the current one.
        accept: 'application/json',
        ...(init?.body ? { 'content-type': 'application/json' } : {}),
        ...init?.headers,
      },
    })
  } catch (cause) {
    // A transport failure. Distinguished from an HTTP error because the two call
    // for different advice: this one means "is the agent running", the other
    // means "the agent answered, and the answer was no".
    throw new ApiError(
      0,
      'unreachable',
      cause instanceof Error ? cause.message : 'the agent could not be reached',
    )
  }

  if (response.status === 204 || response.headers.get('content-length') === '0') {
    return undefined as T
  }

  const text = await response.text()
  if (!response.ok) {
    let code = `http-${response.status}`
    let detail = text.trim() || response.statusText
    try {
      const body = JSON.parse(text) as ErrorBody
      if (typeof body.code === 'string') code = body.code
      if (typeof body.message === 'string') detail = body.message
    } catch {
      // Not JSON. The raw text is the most informative thing available, and the
      // status-based code is already set.
    }
    throw new ApiError(response.status, code, detail)
  }

  // A successful response with an empty body is possible for a route that is
  // declared to return JSON but has nothing to say.
  if (!text) return undefined as T
  return JSON.parse(text) as T
}

/** Issues a GET. */
export function get<T>(path: string, signal?: AbortSignal): Promise<T> {
  return request<T>(path, { method: 'GET', signal })
}

/** Issues a POST with an optional JSON body. */
export function post<T>(path: string, body?: unknown): Promise<T> {
  return request<T>(path, {
    method: 'POST',
    body: body === undefined ? undefined : JSON.stringify(body),
  })
}

/** Issues a PATCH with a JSON body. */
export function patch<T>(path: string, body: unknown): Promise<T> {
  return request<T>(path, { method: 'PATCH', body: JSON.stringify(body) })
}

/** Issues a DELETE with an optional JSON body. */
export function del<T>(path: string, body?: unknown): Promise<T> {
  return request<T>(path, {
    method: 'DELETE',
    body: body === undefined ? undefined : JSON.stringify(body),
  })
}
