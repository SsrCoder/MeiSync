/**
 * Why is this document stored base64 / in full-sync mode?
 *
 * MeiSync resolves the effective content type of every write from the request `Content-Type`
 * **and** the file name (a specific declared type wins, a generic `application/octet-stream` /
 * `application/x-www-form-urlencoded` defers to the extension), and that decides whether the
 * bytes are stored verbatim in `Y.Text("content")` (incremental sync) or base64 encoded (full
 * sync). This script prints that decision for a live document.
 *
 * Usage:
 *   npm run explain -- --target http://127.0.0.1:8080 --path /private/alice/notes/todo.md
 *   npm run explain -- --target http://127.0.0.1:8080 --user alice --path /private/alice/notes/todo.md
 *   npm run explain -- --table        # just the rule
 */
import { MeiSyncApi } from '../src/meisync/api'

function arg(name: string, fallback: string): string {
  const index = process.argv.indexOf(`--${name}`)
  return index !== -1 && process.argv[index + 1] ? process.argv[index + 1] : fallback
}
const has = (name: string) => process.argv.includes(`--${name}`)

const RULE = `
effective type = the declared Content-Type, unless it is generic — then the extension decides
                  (meisync_core::resolve_content_type)

  declared Content-Type                     name          effective          sync mode
  ----------------------------------------  ------------  -----------------  ------------
  text/markdown; charset=utf-8              notes.md      text/markdown     incremental
  application/octet-stream   (Apifox)       notes.md      text/markdown     incremental
  application/x-www-form-urlencoded (curl)   notes.md      text/markdown     incremental
  binary/octet-stream        (some SDKs)    notes.md      text/markdown     incremental
  application/octet-stream                   logo.png      image/png         full
  application/octet-stream                   blob          octet-stream      full
  image/png                                  notes.md      image/png         full
`

/** Mirrors `meisync_core::content_encoding`: which effective types are stored verbatim. */
const TEXTUAL = new Set([
  'application/json',
  'application/x-ndjson',
  'application/yaml',
  'application/x-yaml',
  'application/toml',
  'application/xml',
  'application/javascript',
  'application/ecmascript',
  'application/typescript',
  'application/x-sh',
  'application/csv',
  'application/x-sql',
  'application/graphql',
])
const isTextual = (contentType: string | undefined): boolean => {
  const ct = (contentType ?? '').split(';')[0].trim().toLowerCase()
  return ct.startsWith('text/') || TEXTUAL.has(ct)
}

function decodeBase64(text: string): Uint8Array | null {
  try {
    const binary = atob(text.replace(/\s+/g, ''))
    return Uint8Array.from(binary, (char) => char.charCodeAt(0))
  } catch {
    return null
  }
}

async function main(): Promise<void> {
  if (has('--table')) {
    console.log(RULE)
    return
  }

  const target = arg('target', 'http://127.0.0.1:8080')
  const user = arg('user', '')
  const path = arg('path', '')
  if (!path) {
    console.log(RULE)
    console.error('usage: explain --target <url> [--user <name> --password <pw>] --path <namespace path>')
    process.exit(2)
  }

  const api = new MeiSyncApi({ baseUrl: target })
  const password = arg('password', 'secret123')
  if (user) {
    const auth = await api.login(user, password)
    api.token = auth.token
  } else {
    const raw = arg('token', '')
    if (!raw) throw new Error('pass --user/--password or --token')
    api.token = raw
  }

  const document = (await api.document(path)) as {
    content: string
    metadata: { content_type?: string; sync_mode?: string; size?: number; version?: number; kind: string }
  }
  const { content_type: contentType, sync_mode: syncMode, kind } = document.metadata
  const text = document.content ?? ''
  // The encoding follows the *effective* content type; `sync_mode` only mirrors it and is never
  // downgraded, so a document written as text and later PUT with a binary type is base64 while
  // still reporting `incremental`.
  const storedBase64 = kind === 'file' && text.length > 0 && !isTextual(contentType)

  console.log(`\n${path}`)
  console.log(`  effective content_type : ${contentType ?? '(collection)'}`)
  console.log(`  sync_mode             : ${syncMode ?? '(collection)'}`)
  console.log(`  Y.Text("content")     : ${text.length} chars`)

  if (storedBase64) {
    const bytes = decodeBase64(text)
    console.log(`  decoded               : ${bytes ? `${bytes.length} bytes` : 'not valid base64'}`)
    console.log(`  first bytes           : ${bytes ? Array.from(bytes.slice(0, 8)).map((b) => b.toString(16).padStart(2, '0')).join(' ') : '-'}`)
    console.log('\n  → stored base64 (full sync). The effective type above is not text-ish,')
    console.log('    so the node keeps only the current snapshot and syncs whole states.')
    console.log('    A generic Content-Type on this file would have been overridden by the')
    console.log('    extension; a *specific* binary type wins, so re-uploading with')
    console.log('    `content-type: text/markdown` stores it verbatim (the sync mode itself')
    console.log('    is only ever upgraded, so writing to a new path restores incremental sync).')
  } else {
    console.log('  → stored verbatim (incremental sync): Yjs updates are merged and the')
    console.log('    update log is replayed on top of the snapshot.')
  }
  console.log(RULE)
}

main().catch((error) => {
  console.error(error)
  process.exit(1)
})