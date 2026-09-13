/**
 * A browser smoke test against a **real** agent.
 *
 * # Why this is not a component test
 *
 * Everything this interface can get wrong that a unit test cannot see lives at the
 * boundary with the agent: whether the session cookie is actually accepted,
 * whether the NDJSON stream is readable through a real `ReadableStream`, whether a
 * page renders at all once the queries resolve, whether `localStorage`-driven
 * language selection takes effect. jsdom and a mocked `fetch` would exercise none
 * of it, because the bugs are in the integration rather than in the logic.
 *
 * # Running it
 *
 * It needs an agent, and it is skipped when there is not one — a test that fails
 * for an environment reason teaches people to ignore it.
 *
 * ```bash
 * # On the agent host
 * proxyctl token issue --principal smoke --role admin
 * proxyctl agent run --config /etc/proxy-agent/config.toml
 *
 * # Here
 * PROXYCTL_SMOKE_TOKEN=<token> pnpm test:smoke
 * ```
 */

import { expect, test, type Page } from '@playwright/test'

/** Where the interface is served. */
const BASE = process.env.PROXYCTL_SMOKE_URL ?? 'http://127.0.0.1:9090'

/** The token to sign in with. Absent means the suite is skipped. */
const TOKEN = process.env.PROXYCTL_SMOKE_TOKEN ?? ''

/** Whether an agent answered. Resolved once, before any test runs. */
const reachable = await (async () => {
  try {
    const response = await fetch(`${BASE}/api/v1/session`)
    // 401 is the right answer for an unauthenticated probe, and it proves an agent
    // is there. Anything that is not a response means there is not.
    return response.status === 401 || response.status === 200
  } catch {
    return false
  }
})()

test.skip(!reachable, `no agent answered at ${BASE}`)
test.skip(!TOKEN, 'PROXYCTL_SMOKE_TOKEN is not set')

/** Signs in, so the application renders rather than the sign-in page. */
async function signIn(page: Page) {
  await page.goto(BASE)
  await page.getByLabel(/API token/i).fill(TOKEN)
  await page.getByRole('button', { name: /Sign in/i }).click()
  await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({ timeout: 15_000 })
}

test.describe('the interface, against a real agent', () => {
  test('serves the application shell rather than the placeholder', async ({ page }) => {
    await page.goto(BASE)
    // The placeholder says the bundle is missing. Its absence is what proves the
    // real interface was built and embedded.
    await expect(page.locator('body')).not.toContainText('front end was not built')
    await expect(page.getByRole('button', { name: /Sign in/i })).toBeVisible()
  })

  test('signs in and renders the overview', async ({ page }) => {
    await signIn(page)

    // Wait for a value the agent supplies rather than for the shell. The
    // capabilities arrive from `/system`, so their presence is what proves a query
    // resolved rather than the page merely mounting with its skeleton showing.
    await expect(page.getByText('Capabilities')).toBeVisible({ timeout: 20_000 })
    await expect(page.getByText('tun_device')).toBeVisible({ timeout: 20_000 })

    // The role label comes from the session, and is what the layout gates on.
    await expect(page.getByText('Administrator')).toBeVisible()
  })

  test('reports the event stream as live', async ({ page }) => {
    await signIn(page)
    // The stream is opened by the layout and announces itself with a heartbeat.
    await expect(page.getByText(/^(Live|实时)$/)).toBeVisible({ timeout: 20_000 })
  })

  test('navigates every page without an error state', async ({ page }) => {
    await signIn(page)

    for (const [route, heading] of [
      ['mihomo', 'Kernel'],
      ['configs', 'Configurations'],
      ['subscriptions', 'Subscriptions'],
      ['connections', 'Connections'],
      ['logs', 'Logs'],
      ['system', 'System'],
      ['doctor', 'Doctor'],
    ] as const) {
      // A full load rather than a client-side click, because a direct load is what
      // exercises the agent's single-page fallback.
      await page.goto(`${BASE}/${route}`)
      await expect(
        page.getByRole('heading', { name: heading }),
        `${route} should render`,
      ).toBeVisible({ timeout: 15_000 })

      // "Could not be reached" is the one thing that must never appear: it means
      // the page rendered while its data did not.
      await expect(page.locator('body')).not.toContainText('could not be reached')
    }
  })

  test('shows a 404 asset as a 404 rather than the entry point', async ({ page }) => {
    const response = await page.goto(`${BASE}/definitely-missing.js`)
    expect(response?.status()).toBe(404)
  })

  test('switches language, and remembers the choice', async ({ page }) => {
    await signIn(page)

    // The button is labelled with the language it switches *to*, not with the
    // word "Language", so it reads as "中文" while English is active.
    await page.getByRole('button', { name: '中文' }).click()
    await expect(page.getByRole('heading', { name: '概览' })).toBeVisible({ timeout: 10_000 })

    // A reload proves the choice was stored rather than held in memory: the
    // initial language is read from `localStorage` before the first render.
    await page.reload()
    await expect(page.getByRole('heading', { name: '概览' })).toBeVisible({ timeout: 15_000 })

    // The document's language follows, which is what a screen reader reads.
    expect(await page.locator('html').getAttribute('lang')).toBe('zh-CN')
  })

  test('signs out and returns to the sign-in page', async ({ page }) => {
    await signIn(page)
    await page.getByRole('button', { name: /Sign out/i }).click()
    await expect(page.getByRole('button', { name: /Sign in/i })).toBeVisible({ timeout: 10_000 })
  })

  test('renders without a horizontal scrollbar at a narrow width', async ({ page }) => {
    await page.setViewportSize({ width: 1024, height: 768 })
    await signIn(page)
    // A layout that overflows is the most common way a table-heavy page breaks,
    // and it is invisible in a screenshot taken at a desktop width.
    const overflows = await page.evaluate(
      () => document.documentElement.scrollWidth > document.documentElement.clientWidth + 1,
    )
    expect(overflows, 'the overview should not scroll horizontally at 1024px').toBe(false)
  })
})
