/**
 * The application's entry point.
 *
 * # Why the providers are nested in this order
 *
 * `QueryClientProvider` is outermost because the session provider's own calls go
 * through it. `SessionProvider` comes next because everything below it may need to
 * know who is signed in. `TooltipProvider` and the router are innermost because
 * they render components that use queries, and nothing here fetches on its own.
 */

import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { QueryClientProvider } from '@tanstack/react-query'
import { BrowserRouter } from 'react-router-dom'

import { App } from './App'
import { SessionProvider } from './components/session-provider'
import { TooltipProvider } from './components/ui/tooltip'
import './lib/i18n'
import { queryClient } from './lib/query'
import './index.css'

const container = document.getElementById('root')
if (!container) {
  // The one unrecoverable state in this bundle: without the mount point there is
  // nowhere to render, and the cause is a mismatch between `index.html` and this
  // file rather than anything a reader could have done.
  throw new Error('index.html has no #root element')
}

createRoot(container).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <SessionProvider>
        <TooltipProvider delayDuration={200}>
          <BrowserRouter>
            <App />
          </BrowserRouter>
        </TooltipProvider>
      </SessionProvider>
    </QueryClientProvider>
  </StrictMode>,
)
