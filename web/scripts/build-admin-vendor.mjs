/**
 * Build the admin console vendor bundle (`crates/admin/web/vendor/bundle.js`).
 *
 * The admin console itself has no build step — the bundle is produced here, committed, and
 * embedded into the binary with `include_str!`. Run from `web/`:
 *
 *   npm install            # once; the editor toolchain lives in devDependencies
 *   npm run build:admin-vendor
 */
import { fileURLToPath } from 'node:url'
import * as esbuild from 'esbuild'

const webDir = fileURLToPath(new URL('..', import.meta.url))
const entry = fileURLToPath(new URL('../../crates/admin/web/vendor/entry.js', import.meta.url))
const out = fileURLToPath(new URL('../../crates/admin/web/vendor/bundle.js', import.meta.url))

await esbuild.build({
  entryPoints: [entry],
  // The entry lives outside `web/`, so point resolution at this package's node_modules.
  nodePaths: [`${webDir}node_modules`],
  bundle: true,
  minify: true,
  format: 'esm',
  target: 'es2022',
  outfile: out,
  legalComments: 'none',
  logLevel: 'info',
})
