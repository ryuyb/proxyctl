import path from 'node:path'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

/**
 * The agent's default address during development.
 *
 * Overridable with `PROXYCTL_DEV_TARGET` so a developer can point the interface
 * at an agent on another machine. The default is plain HTTP on the loopback: the
 * development server talks to a real agent rather than a mock, because the
 * interface's hardest problems — auth, streaming, roles — are exactly the ones a
 * mock would get wrong by construction.
 */
const target = process.env.PROXYCTL_DEV_TARGET ?? 'http://127.0.0.1:9090'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    // `@/` rather than a forest of `../../`. shadcn's generator writes imports in
    // this form, so the alias is also what keeps generated components from
    // needing edits after they are added.
    alias: { '@': path.resolve(import.meta.dirname, './src') },
  },
  server: {
    // The development server is bound to the loopback only. It proxies requests
    // that carry a session cookie to a real agent, so exposing it on a LAN
    // address would put a credentialed interface on the network without the
    // agent's own token requirement applying to it.
    host: '127.0.0.1',
    proxy: {
      // `changeOrigin` must stay false. The agent's origin check compares
      // `Origin` against `Host`, so rewriting `Host` to the target's would make
      // every state-changing request look cross-origin and be refused.
      '/api': { target, changeOrigin: false },
      // The event stream: NDJSON over chunked HTTP, not a WebSocket. Buffering
      // here would defeat it entirely, so the response is marked uncacheable and
      // untransformed. Whether that is enough is verified against a real agent
      // rather than assumed.
      '/ws': {
        target,
        changeOrigin: false,
        configure: (proxy) => {
          proxy.on('proxyRes', (proxyRes) => {
            proxyRes.headers['cache-control'] = 'no-cache, no-transform'
          })
        },
      },
      // The upstream dashboard's own endpoint, served by the agent under an
      // ADMIN-only prefix.
      '/clash-api': { target, changeOrigin: false },
    },
  },
  build: {
    // Straight into the directory `crates/interfaces/build.rs` walks, so a
    // release build embeds what this produced with no copying step between.
    outDir: 'dist',
    emptyOutDir: true,
    sourcemap: false,
    rollupOptions: {
      output: {
        // Assets are content-hashed, and the agent serves them as immutable, so
        // a new build cannot be served a stale chunk from a browser cache.
        entryFileNames: 'assets/[name]-[hash].js',
        chunkFileNames: 'assets/[name]-[hash].js',
        assetFileNames: 'assets/[name]-[hash][extname]',
        // The dependency vendors are split out so that changing this bundle's own
        // code does not invalidate the large third-party chunk. They change when
        // the lockfile changes, which is far less often than the application does,
        // and every operator re-downloading React on each deploy is the cost this
        // avoids.
        //
        // A function rather than an object map: that is the form the bundler takes,
        // and the object form is silently not applied rather than being an error.
        manualChunks(id) {
          if (!id.includes('node_modules')) return undefined
          if (/[\\/]node_modules[\\/](react|react-dom|react-router|react-router-dom)[\\/]/.test(id)) {
            return 'react'
          }
          if (/[\\/]node_modules[\\/](i18next|react-i18next)[\\/]/.test(id)) {
            return 'i18n'
          }
          return undefined
        },
      },
    },
  },
})
