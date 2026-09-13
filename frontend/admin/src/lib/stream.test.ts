/**
 * The NDJSON reader.
 *
 * # What is worth testing here
 *
 * The reader's failure modes are all about boundaries, and every one of them is
 * invisible in a test that hands it a whole body at once:
 *
 * * a line split across two chunks,
 * * a multi-byte character split across two chunks,
 * * several lines arriving in one chunk,
 * * a body that ends without a trailing newline.
 *
 * So the tests drive a fake `fetch` that yields byte arrays chosen to land on those
 * boundaries, rather than a string the implementation could have handled by
 * accident.
 */

import { afterEach, describe, expect, it, vi } from 'vitest'

import { readLines } from './stream'

/** A fake response whose body yields the given chunks. */
function respond(chunks: Uint8Array[]): void {
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(chunk)
      controller.close()
    },
  })

  vi.stubGlobal('fetch', () =>
    Promise.resolve(new Response(stream, { status: 200 })),
  )
}

/** Encodes a string as UTF-8 bytes. */
function bytes(text: string): Uint8Array {
  return new TextEncoder().encode(text)
}

/** Runs a read and collects everything it produced. */
async function collect(chunks: Uint8Array[]) {
  respond(chunks)
  const values: unknown[] = []
  const malformed: string[] = []
  let ended: string | undefined

  await readLines<unknown>('/test', {
    signal: new AbortController().signal,
    onLine: ({ value }) => values.push(value),
    onMalformed: (raw) => malformed.push(raw),
    onEnd: (reason) => {
      ended = reason
    },
  })

  return { values, malformed, ended }
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('readLines', () => {
  it('reads one value per line', async () => {
    const { values } = await collect([bytes('{"a":1}\n{"a":2}\n')])
    expect(values).toEqual([{ a: 1 }, { a: 2 }])
  })

  /**
   * The central case. A `read()` returning half a line must not be parsed as a
   * line — the naive implementation fails here, and only here.
   */
  it('reassembles a line split across chunks', async () => {
    const { values, malformed } = await collect([
      bytes('{"message":"hel'),
      bytes('lo"}\n'),
    ])
    expect(values).toEqual([{ message: 'hello' }])
    expect(malformed).toEqual([])
  })

  /**
   * A multi-byte character split across chunks. Without `TextDecoder({stream:true})`
   * each half decodes to U+FFFD and the text is corrupted rather than lost, so this
   * is asserted on the decoded value.
   */
  it('reassembles a multi-byte character split across chunks', async () => {
    const encoded = bytes('{"m":"内核日志"}\n')
    const cut = 12 // deliberately inside a multi-byte sequence

    const { values, malformed } = await collect([encoded.slice(0, cut), encoded.slice(cut)])
    expect(malformed).toEqual([])
    expect(values).toEqual([{ m: '内核日志' }])
  })

  it('reads several lines arriving in one chunk', async () => {
    const { values } = await collect([bytes('{"n":1}\n{"n":2}\n{"n":3}\n')])
    expect(values).toEqual([{ n: 1 }, { n: 2 }, { n: 3 }])
  })

  /**
   * A body that ends without a trailing newline still has one complete value in
   * it, and losing it would drop the last thing the agent said.
   */
  it('keeps a final line with no trailing newline', async () => {
    const { values } = await collect([bytes('{"n":1}\n{"n":2}')])
    expect(values).toEqual([{ n: 1 }, { n: 2 }])
  })

  it('reports a malformed line instead of dropping it silently', async () => {
    const { values, malformed } = await collect([bytes('not json\n{"ok":true}\n')])
    expect(values).toEqual([{ ok: true }])
    expect(malformed).toEqual(['not json'])
  })

  it('ignores blank lines', async () => {
    const { values, malformed } = await collect([bytes('\n\n{"a":1}\n\n')])
    expect(values).toEqual([{ a: 1 }])
    expect(malformed).toEqual([])
  })

  /** A heartbeat is a real event, so it must reach the caller. */
  it('delivers a heartbeat line', async () => {
    const { values } = await collect([bytes('{"seq":1,"kind":"heartbeat","at":1,"data":{}}\n')])
    expect(values).toEqual([{ seq: 1, kind: 'heartbeat', at: 1, data: {} }])
  })

  /** The contract is a sequence, so order is asserted rather than assumed. */
  it('preserves order across many chunks', async () => {
    const chunks = Array.from({ length: 20 }, (_, index) => bytes(`{"n":${index}}\n`))
    const { values } = await collect(chunks)
    expect(values).toEqual(Array.from({ length: 20 }, (_, index) => ({ n: index })))
  })

  /** A body that ends cleanly is not an error. */
  it('reports a clean end', async () => {
    const { ended } = await collect([bytes('{"a":1}\n')])
    expect(ended).toBe('closed')
  })

  /**
   * A non-200 response ends the stream with an error rather than being parsed as
   * data: a 401 body is not an event.
   */
  it('does not parse an error response as events', async () => {
    vi.stubGlobal('fetch', () =>
      Promise.resolve(new Response('{"code":"unauthorized"}', { status: 401 })),
    )
    const values: unknown[] = []
    let ended: string | undefined
    await readLines('/test', {
      signal: new AbortController().signal,
      onLine: ({ value }) => values.push(value),
      onEnd: (reason) => {
        ended = reason
      },
    })
    expect(values).toEqual([])
    expect(ended).toBe('error')
  })

  /** An aborted read reports the abort rather than a failure. */
  it('reports an abort as an abort', async () => {
    const controller = new AbortController()
    controller.abort()
    vi.stubGlobal('fetch', () => Promise.reject(new DOMException('aborted', 'AbortError')))

    let ended: string | undefined
    await readLines('/test', {
      signal: controller.signal,
      onLine: () => {},
      onEnd: (reason) => {
        ended = reason
      },
    })
    expect(ended).toBe('aborted')
  })
})
