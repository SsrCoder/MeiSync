import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

/**
 * The dev server proxies `/api`, `/dav` and `/ysync` to a local MeiSync node. That keeps the
 * browser same-origin (the node ships no CORS layer) and makes `ws://127.0.0.1:8080/ysync/...`
 * reachable as `/ysync/...`.
 *
 * Override the node address with `MEISYNC_TARGET=host:port` when it is not 127.0.0.1:8080.
 */
const target = process.env.MEISYNC_TARGET ?? 'http://127.0.0.1:8080'

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': { target, changeOrigin: true },
      '/dav': { target, changeOrigin: true },
      '/s3': { target, changeOrigin: true },
      // `ws: true` turns the entry into an upgrade proxy; the target itself stays http://.
      '/ysync': { target, changeOrigin: true, ws: true },
    },
  },
  test: {
    environment: 'node',
    include: ['tests/**/*.test.ts'],
  },
})