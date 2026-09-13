/**
 * The application shell: navigation, the session's identity, and the language
 * control.
 *
 * # Why the navigation is a fixed column
 *
 * This is an operator's tool, not a document. Its pages are visited repeatedly
 * while something is being diagnosed, and a horizontal bar that has to be
 * re-opened costs a click on every hop. The column also leaves the whole width
 * below for tables — which is what most of these pages are.
 *
 * # Why there is no breadcrumb
 *
 * The tree is one level deep. A breadcrumb would always read "Proxy Control /
 * Kernel", which tells a reader nothing they do not already know from the
 * highlighted item.
 */

import { useTranslation } from 'react-i18next'
import { NavLink, Outlet } from 'react-router-dom'
import {
  Activity,
  FileCode2,
  LayoutDashboard,
  List,
  LogOut,
  ScrollText,
  Server,
  Stethoscope,
  Waypoints,
} from 'lucide-react'

import { LanguageToggle } from '@/components/language-toggle'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { useSession } from '@/lib/session'
import { useEventStream } from '@/lib/use-event-stream'

/** One navigation entry. */
interface Entry {
  to: string
  labelKey: string
  icon: typeof LayoutDashboard
  /** Whether the route matches only exactly, which the index route needs. */
  end?: boolean
}

const ENTRIES: Entry[] = [
  { to: '/', labelKey: 'nav.overview', icon: LayoutDashboard, end: true },
  { to: '/mihomo', labelKey: 'nav.mihomo', icon: Activity },
  { to: '/configs', labelKey: 'nav.configs', icon: FileCode2 },
  { to: '/subscriptions', labelKey: 'nav.subscriptions', icon: List },
  { to: '/connections', labelKey: 'nav.connections', icon: Waypoints },
  { to: '/logs', labelKey: 'nav.logs', icon: ScrollText },
  { to: '/system', labelKey: 'nav.system', icon: Server },
  { to: '/doctor', labelKey: 'nav.doctor', icon: Stethoscope },
]

export function Layout() {
  const { t } = useTranslation()
  const { session, signOut } = useSession()

  // The stream is opened once, here, for the whole application: it is what keeps
  // every page's data current, and opening a second subscription per page would
  // multiply the connections the agent holds for no additional information.
  const stream = useEventStream()

  return (
    <div className="flex min-h-screen bg-background">
      <aside className="flex w-60 shrink-0 flex-col border-r bg-card">
        <div className="border-b px-4 py-4">
          <div className="text-base font-semibold tracking-tight">{t('app.name')}</div>
          <div className="text-xs text-muted-foreground">{t('app.tagline')}</div>
        </div>

        <nav className="flex-1 space-y-0.5 p-2">
          {ENTRIES.map((entry) => (
            <NavLink
              key={entry.to}
              to={entry.to}
              end={entry.end}
              className={({ isActive }) =>
                cn(
                  'flex items-center gap-2.5 rounded-md px-2.5 py-2 text-sm transition-colors',
                  isActive
                    ? 'bg-accent font-medium text-accent-foreground'
                    : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground',
                )
              }
            >
              <entry.icon className="size-4 shrink-0" />
              {t(entry.labelKey)}
            </NavLink>
          ))}
        </nav>

        <div className="space-y-3 border-t p-3">
          <StreamIndicator state={stream} />
          <LanguageToggle />
          <div className="flex items-center justify-between gap-2">
            <div className="min-w-0">
              <div className="truncate text-xs font-medium" title={session?.principal}>
                {session?.principal}
              </div>
            </div>
            <Button
              variant="ghost"
              size="icon"
              className="size-8 shrink-0"
              title={t('nav.signOut')}
              aria-label={t('nav.signOut')}
              onClick={() => void signOut()}
            >
              <LogOut className="size-4" />
            </Button>
          </div>
        </div>
      </aside>

      <main className="min-w-0 flex-1">
        <Outlet />
      </main>
    </div>
  )
}

/**
 * Whether the event stream is connected.
 *
 * # Why this is in the shell rather than on a page
 *
 * A dropped stream is not a page's problem: it means every panel on screen may be
 * showing something that has moved on. That is a property of the whole window, so
 * it is stated once, where it is always visible, rather than repeated per page.
 */
function StreamIndicator({ state }: { state: 'connecting' | 'live' | 'reconnecting' }) {
  const { t } = useTranslation()
  const label =
    state === 'live'
      ? t('events.connected')
      : state === 'reconnecting'
        ? t('events.reconnecting')
        : t('common.loading')

  return (
    <div className="flex items-center gap-2 text-xs text-muted-foreground">
      <span
        className={cn(
          'size-1.5 rounded-full',
          state === 'live' && 'bg-status-ok',
          state === 'reconnecting' && 'bg-status-warn',
          state === 'connecting' && 'bg-status-unknown',
        )}
      />
      {label}
    </div>
  )
}
