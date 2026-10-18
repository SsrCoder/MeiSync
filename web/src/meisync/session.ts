import * as Y from 'yjs'
import { MeiSyncProvider, ysyncUrl } from './provider'
import { CONTENT_KEY, createYTextBinding, type YTextBinding } from './binding'

export interface LocalUser {
  name: string
  color: string
}

export interface SessionOptions {
  /** Session token from `/api/auth/login`. */
  token: string
  /** Namespace path of the opened file, e.g. `/private/alice/notes/todo.md`. */
  path: string
  user: LocalUser
  /** Origin used to build the `ws(s)://…/ysync/…` URL. */
  origin?: string
  onLog?: (message: string) => void
}

export interface Session {
  doc: Y.Doc
  provider: MeiSyncProvider
  binding: YTextBinding
  path: string
  /** The shared `Y.Text("content")` of the opened file. */
  ytext: Y.Text
  destroy(): void
}

/**
 * One browser tab = one Y.Doc bound to one MeiSync file. Two tabs (or two browsers) opening the
 * same path are the collaborative pair the server is meant to sync.
 */
export function createSession(options: SessionOptions): Session {
  const { token, path, user, origin = globalThis.location?.origin ?? 'http://127.0.0.1:5173' } = options
  const onLog = options.onLog ?? (() => {})
  const doc = new Y.Doc()
  const provider = new MeiSyncProvider({ url: ysyncUrl(origin, path), doc, token })
  const binding = createYTextBinding({
    ydoc: doc,
    awareness: provider.awareness,
    key: CONTENT_KEY,
    user,
    onLog,
  })

  provider.on('status', (status) => onLog(`ysync status: ${status}`))
  provider.on('synced', (synced) => onLog(synced ? 'initial sync complete' : 'no longer in sync'))
  provider.on('denied', (reason) => onLog(`access denied: ${reason}`))
  provider.on('moved', (newPath) => onLog(`document moved to ${newPath}`))
  provider.on('error', (message) => onLog(`ysync: ${message}`))

  return {
    doc,
    provider,
    binding,
    path,
    ytext: doc.getText(CONTENT_KEY),
    destroy() {
      binding.destroy()
      provider.destroy()
      doc.destroy()
    },
  }
}

/** Stable, readable colour per tab / user so remote carets are distinguishable. */
const PALETTE = [
  '#6ea8fe',
  '#f28b82',
  '#81c995',
  '#fdd663',
  '#c58af9',
  '#78d9ec',
  '#ff9e6d',
  '#b0bec5',
]

export function colorFor(seed: string): string {
  let hash = 0
  for (let i = 0; i < seed.length; i++) hash = (hash * 31 + seed.charCodeAt(i)) >>> 0
  return PALETTE[hash % PALETTE.length]
}