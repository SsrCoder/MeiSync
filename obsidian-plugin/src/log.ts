import type { LogEntry, LogLevel } from './types'

/**
 * Query-string tokens as they appear in WebSocket URLs (`?token=…` / `&token=…`).
 *
 * The log is shown in a modal and copied to the clipboard, so a bearer token that slipped into a
 * message (for example through a socket error carrying its URL) must never survive here.
 */
const TOKEN_IN_URL = /([?&]token=)[^&#\s"']*/gi

export class SyncLog {
  private entries: LogEntry[] = []
  private listeners = new Set<() => void>()

  constructor(private readonly limit = 800) {}

  add(level: LogLevel, raw: string): void {
    const message = raw.replace(TOKEN_IN_URL, '$1…')
    const entry: LogEntry = { at: Date.now(), level, message }
    this.entries.push(entry)
    if (this.entries.length > this.limit) {
      this.entries.splice(0, this.entries.length - this.limit)
    }
    if (level === 'error') console.error('[MeiSync]', message)
    else if (level === 'warn') console.warn('[MeiSync]', message)
    else console.debug('[MeiSync]', message)
    for (const listener of this.listeners) listener()
  }

  info(message: string): void {
    this.add('info', message)
  }

  warn(message: string): void {
    this.add('warn', message)
  }

  error(message: string): void {
    this.add('error', message)
  }

  all(): LogEntry[] {
    return [...this.entries]
  }

  clear(): void {
    this.entries = []
    for (const listener of this.listeners) listener()
  }

  onChange(listener: () => void): () => void {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  asText(): string {
    return this.entries
      .map((entry) => `${new Date(entry.at).toISOString()} ${entry.level.toUpperCase()} ${entry.message}`)
      .join('\n')
  }
}
