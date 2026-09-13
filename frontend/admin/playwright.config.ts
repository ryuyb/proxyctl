/**
 * The Playwright configuration for the browser smoke test.
 *
 * # Why there is no `webServer` here
 *
 * The other front-end tests run against nothing. This one needs a real agent,
 * which is a machine the test cannot start for itself — it needs a token, a
 * kernel, and a configuration. Starting one would mean the test asserted against
 * whatever the test happened to configure rather than against the deployment an
 * operator actually runs.
 *
 * So it points at `PROXYCTL_SMOKE_URL` and skips when nothing answers there. A
 * test that fails for an environment reason is a test people learn to ignore.
 */

import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  // One worker: the tests share a session cookie jar and a single agent, and
  // running them in parallel would have them signing each other out.
  workers: 1,
  fullyParallel: false,
  // No retries. A retry would hide exactly the flakiness this is meant to catch:
  // a stream that sometimes does not connect, a query that races its own mount.
  retries: 0,
  reporter: [['list']],
  timeout: 60_000,
  expect: { timeout: 15_000 },
  use: {
    ...devices['Desktop Chrome'],
    // Kept on failure, because a layout defect is usually only visible in the
    // screenshot rather than in the assertion that caught it.
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure',
  },
})
