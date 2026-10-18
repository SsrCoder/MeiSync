/**
 * A minimal YSync provider for MeiSync (y-websocket compatible framing, §9/§10).
 *
 * The server is a plain `yrs` sync endpoint, so `y-websocket` would work as well — but this
 * client is meant to *test* the node, so the framing is spelled out here and the two application
 * level extensions MeiSync adds on top of the standard protocol are surfaced as events:
 *
 *   * `auth denied` + reason          → `denied` event (e.g. the file was deleted while open)
 *   * `custom(42, <new path>)`        → `moved` event (§29 rename / move notification)
 *
 * Wire format (see `crates/ysync/src/lib.rs` and yrs' `sync::protocol`):
 *
 *   [messageType varuint] [payload]
 *     0 sync      → 0 step1 | 1 step2 | 2 update
 *     1 awareness → len-prefixed awareness payload
 *     2 auth      → 0 denied + string reason | 1 granted
 *     3 queryAwareness
 *    42 custom    → len-prefixed payload  (MSG_DOC_MOVED)
 */
import * as Y from 'yjs'
import * as syncProtocol from 'y-protocols/sync'
import { Awareness, applyAwarenessUpdate, encodeAwarenessUpdate, removeAwarenessStates } from 'y-protocols/awareness'
import * as encoding from 'lib0/encoding'
import * as decoding from 'lib0/decoding'

export const MESSAGE_SYNC = 0
export const MESSAGE_AWARENESS = 1
export const MESSAGE_AUTH = 2
export const MESSAGE_QUERY_AWARENESS = 3
/** `meisync_ysync::MSG_DOC_MOVED`. */
export const MESSAGE_DOC_MOVED = 42
export const AUTH_PERMISSION_DENIED = 0
export const AUTH_PERMISSION_GRANTED = 1

export type ProviderStatus = 'connecting' | 'connected' | 'disconnected' | 'denied'

export interface ProviderOptions {
  /** Absolute WebSocket URL of the YSync endpoint, e.g. `ws://127.0.0.1:8080/ysync/private/a/x.md`. */
  url: string
  doc: Y.Doc
  /** Session token; sent as `?token=` because browsers cannot set WS headers. */
  token?: string
  awareness?: Awareness
  /** Extra query parameters appended to the URL. */
  params?: Record<string, string>
  /** Injectable for tests / headless runs. */
  WebSocketImpl?: typeof WebSocket
  /** Disable the automatic connect (useful to build the URL first). */
  autoConnect?: boolean
}

export interface ProviderEvents {
  status: ProviderStatus
  denied: string
  moved: string
  synced: boolean
  error: string
}

type Listener = (payload: never) => void

export class MeiSyncProvider {
  readonly doc: Y.Doc
  readonly awareness: Awareness
  readonly url: string
  status: ProviderStatus = 'disconnected'
  synced = false
  /** Number of Yjs updates applied from the server (handy in the debug panel). */
  updatesReceived = 0
  updatesSent = 0

  private ws: WebSocket | null = null
  private shouldConnect = false
  private reconnectAttempt = 0
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null
  private listeners = new Map<keyof ProviderEvents, Set<Listener>>()
  private readonly WebSocketImpl: typeof WebSocket
  private readonly params: Record<string, string>
  private readonly token?: string

  constructor(options: ProviderOptions) {
    this.doc = options.doc
    this.awareness = options.awareness ?? new Awareness(options.doc)
    this.WebSocketImpl = options.WebSocketImpl ?? (globalThis.WebSocket as typeof WebSocket)
    this.params = options.params ?? {}
    this.token = options.token
    this.url = withParams(buildUrl(options.url), { ...this.params, ...(this.token ? { token: this.token } : {}) })

    // Everything the server sends for this connection is applied with `this` as the origin, which
    // is exactly how `readSyncMessage` / `applyAwarenessUpdate` are called below.
    this.doc.on('update', this.handleLocalUpdate)
    this.awareness.on('update', this.handleAwarenessUpdate)
    if (options.autoConnect !== false) this.connect()
  }

  // ---------------------------------------------------------------- event emitter

  on<K extends keyof ProviderEvents>(event: K, listener: (payload: ProviderEvents[K]) => void): () => void {
    const set = this.listeners.get(event) ?? new Set()
    set.add(listener as Listener)
    this.listeners.set(event, set)
    return () => set.delete(listener as Listener)
  }

  private emit<K extends keyof ProviderEvents>(event: K, payload: ProviderEvents[K]): void {
    this.listeners.get(event)?.forEach((listener) => (listener as (p: ProviderEvents[K]) => void)(payload))
  }

  // ---------------------------------------------------------------- lifecycle

  connect(): void {
    if (this.ws || this.shouldConnect) return
    this.shouldConnect = true
    this.openSocket()
  }

  /** Close the connection and stop reconnecting. */
  disconnect(): void {
    this.shouldConnect = false
    this.clearReconnect()
    this.setStatus('disconnected')
    this.setSynced(false)
    this.closeSocket()
  }

  destroy(): void {
    this.disconnect()
    // Drop our awareness states so peers (and the server room) stop rendering our cursor.
    removeAwarenessStates(this.awareness, [this.doc.clientID], 'destroy')
    this.doc.off('update', this.handleLocalUpdate)
    this.awareness.off('update', this.handleAwarenessUpdate)
    this.listeners.clear()
  }

  private openSocket(): void {
    if (!this.shouldConnect) return
    this.setStatus('connecting')
    const ws = new this.WebSocketImpl(this.url)
    ws.binaryType = 'arraybuffer'
    this.ws = ws

    ws.onopen = () => {
      if (this.ws !== ws) return
      this.reconnectAttempt = 0
      this.setStatus('connected')
      // Standard handshake: "what do I miss?" + awareness query + our own awareness.
      const encoder = encoding.createEncoder()
      encoding.writeVarUint(encoder, MESSAGE_SYNC)
      syncProtocol.writeSyncStep1(encoder, this.doc)
      encoding.writeVarUint(encoder, MESSAGE_QUERY_AWARENESS)
      this.send(encoding.toUint8Array(encoder))
      this.sendLocalAwareness()
    }

    ws.onmessage = (event: MessageEvent) => {
      if (this.ws !== ws) return
      this.handleMessage(new Uint8Array(event.data as ArrayBuffer))
    }

    ws.onerror = () => {
      this.emit('error', `websocket error on ${this.url}`)
    }

    ws.onclose = (event: CloseEvent) => {
      if (this.ws !== ws) return
      this.ws = null
      this.setSynced(false)
      this.setStatus('disconnected')
      if (this.shouldConnect) this.scheduleReconnect(event.reason ? `: ${event.reason}` : '')
    }
  }

  private closeSocket(): void {
    const ws = this.ws
    this.ws = null
    if (!ws) return
    ws.onopen = null
    ws.onmessage = null
    ws.onerror = null
    ws.onclose = null
    try {
      ws.close()
    } catch {
      /* already closed */
    }
  }

  private scheduleReconnect(reason: string): void {
    this.clearReconnect()
    const delay = Math.min(500 * 2 ** this.reconnectAttempt, 8000)
    this.reconnectAttempt++
    this.emit('error', `disconnected${reason}; reconnecting in ${delay}ms`)
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null
      this.openSocket()
    }, delay)
  }

  private clearReconnect(): void {
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer)
    this.reconnectTimer = null
  }

  private setStatus(status: ProviderStatus): void {
    if (this.status === status) return
    this.status = status
    this.emit('status', status)
  }

  private setSynced(synced: boolean): void {
    if (this.synced === synced) return
    this.synced = synced
    this.emit('synced', synced)
  }

  // ---------------------------------------------------------------- messaging

  private send(data: Uint8Array): void {
    if (!this.ws || this.ws.readyState !== 1) return
    this.ws.send(data)
  }

  /** Local Yjs changes go straight to the server, framed as `sync update`. */
  private handleLocalUpdate = (update: Uint8Array, origin: unknown): void => {
    if (origin === this) return // an update we just received from the server
    const encoder = encoding.createEncoder()
    encoding.writeVarUint(encoder, MESSAGE_SYNC)
    syncProtocol.writeUpdate(encoder, update)
    this.updatesSent++
    this.send(encoding.toUint8Array(encoder))
  }

  /** Local cursor/user changes are relayed verbatim to the room (§10, never persisted). */
  private handleAwarenessUpdate = (
    changes: { added: number[]; updated: number[]; removed: number[] },
    origin: unknown,
  ): void => {
    if (origin === this) return
    this.send(this.encodeAwareness(changes.added.concat(changes.updated, changes.removed)))
  }

  private sendLocalAwareness(): void {
    if (!this.awareness.getLocalState()) return
    this.send(this.encodeAwareness([this.doc.clientID]))
  }

  encodeAwareness(clients: number[]): Uint8Array {
    const encoder = encoding.createEncoder()
    encoding.writeVarUint(encoder, MESSAGE_AWARENESS)
    encoding.writeVarUint8Array(encoder, encodeAwarenessUpdate(this.awareness, clients))
    return encoding.toUint8Array(encoder)
  }

  /**
   * A single WebSocket message may carry several frames: the node answers a fresh connection with
   * `[sync step1, awareness snapshot]` in one binary message (§9 handshake). Keep reading until
   * the buffer is drained.
   */
  private handleMessage(data: Uint8Array): void {
    const decoder = decoding.createDecoder(data)
    while (decoding.hasContent(decoder)) {
      this.readFrame(decoder)
    }
  }

  private readFrame(decoder: decoding.Decoder): void {
    const messageType = decoding.readVarUint(decoder)
    switch (messageType) {
      case MESSAGE_SYNC: {
        const encoder = encoding.createEncoder()
        encoding.writeVarUint(encoder, MESSAGE_SYNC)
        // Applies step2/update to `this.doc` with origin `this` and buffers step1/step2 answers.
        syncProtocol.readSyncMessage(decoder, encoder, this.doc, this)
        if (encoding.length(encoder) > 1) this.send(encoding.toUint8Array(encoder))
        this.setSynced(true)
        this.updatesReceived++
        break
      }
      case MESSAGE_AWARENESS: {
        applyAwarenessUpdate(this.awareness, decoding.readVarUint8Array(decoder), this)
        break
      }
      case MESSAGE_QUERY_AWARENESS: {
        this.sendLocalAwareness()
        break
      }
      case MESSAGE_AUTH: {
        const state = decoding.readVarUint(decoder)
        if (state === AUTH_PERMISSION_DENIED) {
          const reason = decoding.readVarString(decoder)
          // The server ends the session right after this frame; reconnecting would only loop.
          this.shouldConnect = false
          this.clearReconnect()
          this.closeSocket()
          this.setStatus('denied')
          this.emit('denied', reason)
        }
        break
      }
      case MESSAGE_DOC_MOVED: {
        const payload = decoding.readVarUint8Array(decoder)
        this.emit('moved', new TextDecoder().decode(payload))
        break
      }
      default:
        // Unknown extension message: ignore, the document state is authoritative anyway.
        break
    }
  }
}

// --------------------------------------------------------------------------------------
// URL helpers
// --------------------------------------------------------------------------------------

/** `http(s)://host/ysync/<path>` -> `ws(s)://host/ysync/<path>` with each segment encoded. */
export function buildUrl(url: string): string {
  const parsed = new URL(url, typeof location === 'undefined' ? 'http://127.0.0.1:8080' : location.href)
  if (parsed.protocol === 'http:') parsed.protocol = 'ws:'
  if (parsed.protocol === 'https:') parsed.protocol = 'wss:'
  parsed.pathname = parsed.pathname
    .split('/')
    .map((segment) => encodeURIComponent(decodeURIComponent(segment)))
    .join('/')
  parsed.search = ''
  parsed.hash = ''
  return parsed.toString()
}

function withParams(url: string, params: Record<string, string>): string {
  const parsed = new URL(url)
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== '') parsed.searchParams.set(key, value)
  }
  return parsed.toString()
}

/** Namespace path (`/private/alice/notes/todo.md`) -> YSync URL for `origin`. */
export function ysyncUrl(origin: string, path: string): string {
  const base = new URL(origin)
  base.protocol = base.protocol === 'https:' ? 'wss:' : 'ws:'
  const clean = path.replace(/^\/+/, '')
  const encoded = clean
    ? `/${clean.split('/').map(encodeURIComponent).join('/')}`
    : ''
  return `${base.toString().replace(/\/$/, '')}/ysync${encoded}`
}