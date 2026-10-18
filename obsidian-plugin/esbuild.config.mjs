import esbuild from 'esbuild'
import { copyFile, mkdir } from 'node:fs/promises'
import process from 'node:process'

const production = process.argv[2] === 'production'
const outdir = 'dist'

/** `dist/` is a ready-to-copy plugin folder: bundle + manifest + stylesheet. */
const copyPluginFiles = {
  name: 'copy-plugin-files',
  setup(build) {
    build.onEnd(async (result) => {
      if (result.errors.length) return
      await mkdir(outdir, { recursive: true })
      for (const file of ['manifest.json', 'styles.css']) {
        await copyFile(file, `${outdir}/${file}`)
      }
    })
  },
}

const context = await esbuild.context({
  entryPoints: ['src/main.ts'],
  bundle: true,
  external: [
    'obsidian',
    'electron',
    '@codemirror/autocomplete',
    '@codemirror/collab',
    '@codemirror/commands',
    '@codemirror/language',
    '@codemirror/lint',
    '@codemirror/search',
    '@codemirror/state',
    '@codemirror/view',
    '@lezer/common',
    '@lezer/highlight',
    '@lezer/lr',
  ],
  format: 'cjs',
  target: 'es2018',
  logLevel: 'info',
  sourcemap: production ? false : 'inline',
  treeShaking: true,
  outfile: `${outdir}/main.js`,
  plugins: [copyPluginFiles],
})

if (production) {
  await context.rebuild()
  await context.dispose()
} else {
  await context.watch()
}
