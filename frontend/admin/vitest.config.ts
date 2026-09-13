/**
 * The vitest configuration.
 *
 * # Why the include is narrowed
 *
 * Vitest's default glob collects `*.spec.ts` anywhere in the tree, which picks up
 * `e2e/smoke.spec.ts` — a Playwright suite. Vitest then fails on it in a way that
 * looks like a broken test rather than two runners disagreeing about who owns a
 * file. Scoping each runner to its own directory is what keeps that unambiguous:
 * `src/**` is vitest's, `e2e/**` is Playwright's.
 *
 * # Why there is no `environment: 'jsdom'`
 *
 * Nothing under `src/` is a component test: the pure units — the NDJSON reader,
 * the locale trees, the generated-import guard — run fine on Node, and the
 * integration behaviour is covered by the browser suite against a real agent. A
 * jsdom environment would only invite tests that assert on a rendering that never
 * happens in a browser.
 */

import { defineConfig } from 'vitest/config'
import path from 'node:path'

export default defineConfig({
  resolve: {
    // Must match `vite.config.ts` and the tsconfigs, or a test importing `@/…`
    // resolves differently from the application doing the same.
    alias: { '@': path.resolve(import.meta.dirname, './src') },
  },
  test: {
    include: ['src/**/*.test.ts', 'src/**/*.test.tsx'],
    exclude: ['e2e/**', 'node_modules/**', 'dist/**'],
  },
})
