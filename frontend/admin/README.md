# The operator interface

The browser interface the agent embeds. It talks to the agent over the same
versioned HTTP API the CLI does — there is no second path, and nothing here
reaches into the agent's internals.

```text
browser  ──HTTP/JSON──▶  agent  ──unix socket──▶  mihomo
            ▲
            └── this bundle, embedded in the binary
```

## Quick start

```bash
pnpm install          # from frontend/, or from here
pnpm dev              # http://127.0.0.1:5173
pnpm check            # typecheck, lint, test, build — the gate
```

`pnpm dev` proxies to an agent at `http://127.0.0.1:9090`. Point it elsewhere
with `PROXYCTL_DEV_TARGET`. There is no mock: the interface is developed against
a real agent, because auth, streaming, and role visibility are precisely the
things a mock would get wrong by construction.

### Running an agent to develop against

```bash
# On the agent host. `[api] bind` requires a token first — see ADR-010 D9.
proxyctl token issue --principal dev --role admin
proxyctl agent run --config /etc/proxy-agent/config.toml
```

## How this reaches the browser

`crates/interfaces/build.rs` walks `frontend/admin/dist/` at compile time and
generates the file list the agent embeds. Two consequences:

* **The bundle is baked in.** Nothing re-runs `pnpm build` when these sources
  change — Cargo does not know this directory exists. A server binary built before
  a front-end change carries the old interface, and the only symptom is a UI that
  does not match the code. Run `pnpm build` before `cargo build` when in doubt;
  `BUNDLE_MODIFIED` exists so the agent can report how stale it is.
* **An absent bundle is not an error.** A backend-only checkout has no
  `node_modules`, and that must not block the agent. A placeholder page is served
  instead, and `assets::is_placeholder()` says so at startup.

## Layout

```text
src/
├── main.tsx            providers, in dependency order
├── App.tsx             routing, and the signed-in gate
├── components/
│   ├── layout.tsx      sidebar, nav, stream indicator, session
│   ├── page.tsx        PageHeader, ErrorState, CapabilityBadge, …
│   ├── confirm-dialog.tsx
│   ├── language-toggle.tsx
│   ├── session-provider.tsx
│   └── ui/             shadcn components — generated, see the note below
├── lib/
│   ├── api.ts          fetch client: errors, status codes, credentials
│   ├── types.ts        the wire types, mirroring crates/interfaces/src/dto
│   ├── session.ts      session context and the sign-in calls
│   ├── query.ts        the query client and every query key
│   ├── stream.ts       the NDJSON reader
│   ├── use-event-stream.ts
│   ├── format.ts       bytes, durations, timestamps
│   ├── i18n.ts + locales/
│   └── utils.ts        `cn`
└── pages/              one module per route
```

## Things that are easy to get wrong here

**The event stream is not a WebSocket.** `/ws/v1/events` is newline-delimited JSON
over a chunked HTTP response. The path keeps its `ws` name from the design
document; there is no handshake and no framing. `lib/stream.ts` reads it with
`fetch` + `ReadableStream`, and two details are load-bearing: a chunk boundary is
not a line boundary, and a multi-byte character can be split across one.
`lib/stream.test.ts` drives both. *Verified against a real agent: the Vite dev
proxy does not buffer this stream — heartbeats arrive 15s apart, on schedule.*

**A `401` is not an error to render.** It means the session ended, and the honest
answer is the sign-in page. `fetchSession` converts it to `null` rather than
letting every call site special-case it.

**Role gating is cosmetic.** `useIsAdmin()` decides which controls are drawn. The
boundary is the agent: it refuses a write from a read-only session and strips
`uid`/`process`/`process_path` from a connection list before serialising it. A
reader can edit anything in this bundle.

**The connection's process columns are conditional for that reason.** A read-only
session receives a payload without those fields, and drawing three empty columns
would invite the reader to conclude the agent looked and found nothing. A note
says otherwise.

**Every user-facing string goes through i18n**, except the agent's own messages.
A failure reason, a log line, a doctor finding's text: those arrive from the agent
in whatever language it produced them, and translating one would mean a Chinese
reader and an English reader saw different text for the same failure.

**`shadcn add` may write a broken import.** It resolves `@/lib/utils` from
`tsconfig.json`. When it cannot, it does not fail — it writes
`import { cn } from "cn"`, and `cn` is a real package on the registry, so the
build succeeds and the class names are wrong. `lib/components.test.ts` guards this.
If it fires:

```bash
sed -i '' 's|from "cn"|from "@/lib/utils"|' src/components/ui/*.tsx   # macOS
sed -i    's|from "cn"|from "@/lib/utils"|' src/components/ui/*.tsx   # Linux
```

## Why the versions are pinned where they are

The toolchain was scaffolded with `create-vite` in September 2026 and is newer
than the design document assumed: **Vite 8 / Rolldown, React 19, TypeScript 6,
oxlint, Tailwind 4**. Three consequences that are not obvious:

* **`manualChunks` must be a function.** The object form is accepted by the type
  checker as an overload and then silently not applied.
* **`baseUrl` is deprecated.** Path mapping is relative to the config file, which
  is what `baseUrl: "."` said. Leaving it in is a compile error under TypeScript 6.
* **`erasableSyntaxOnly` forbids constructor parameter properties**, so
  `class ApiError { constructor(readonly status: number) {} }` does not compile.
  The fields are declared and assigned explicitly.

Type checking is split across three configs — `app` (browser, `vite/client` only),
`test` (Node plus the browser), `node` (Vite's own config) — because they disagree
about what `globalThis` contains. Giving the application `node` types would turn a
bundling failure into a runtime one.

## Testing

```bash
pnpm test        # vitest: pure units
pnpm typecheck   # tsc -b, across all three projects
pnpm lint        # oxlint
pnpm check       # all of the above, plus a production build
```

`pnpm test` covers the things where a mistake is invisible at review time: locale
key parity and placeholder agreement, the NDJSON reader's chunk and encoding
boundaries, and the generated-component imports. It is scoped to `src/`, because
Vitest's default glob also collects `e2e/`.

### The browser suite

`pnpm test:smoke` drives the built interface against a **real** agent in Chromium.
It is a separate runner and a separate gate, because it needs an agent that the
test cannot start for itself. It skips when none answers.

```bash
# On the agent host
proxyctl token issue --principal smoke --role admin
proxyctl agent run --config /etc/proxy-agent/config.toml

# Here — `pnpm build` first, since the agent serves the *embedded* bundle
pnpm build
PROXYCTL_SMOKE_TOKEN=<token> pnpm test:smoke
```

It is worth running before a release, and after any change to auth, the stream, or
routing. It has already caught one defect a unit test could not: `<html lang>` was
set only in the language toggle's click handler, so a *reloaded* Chinese interface
announced itself as English to a screen reader. The fix moved it onto i18next's
`languageChanged`, which fires on both paths.
