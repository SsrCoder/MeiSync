export type ConflictStrategy = 'duplicate' | 'local-wins' | 'remote-wins' | 'manual'
export type InitialSyncMode = 'merge' | 'prefer-local' | 'prefer-remote'

export interface MeiSyncSettings {
  serverUrl: string
  username: string
  password: string
  token: string
  remoteRoot: string
  vaultNameOverride: string
  deviceName: string
  autoSync: boolean
  syncIntervalSeconds: number
  realtime: boolean
  hybridRealtime: boolean
  syncAttachments: boolean
  syncConfig: boolean
  excludePatterns: string
  conflictStrategy: ConflictStrategy
  initialSync: InitialSyncMode
  maxFileSizeMb: number
  debug: boolean
}

export const DEFAULT_SETTINGS: MeiSyncSettings = {
  serverUrl: '',
  username: '',
  password: '',
  token: '',
  remoteRoot: 'obsidian',
  vaultNameOverride: '',
  deviceName: '',
  autoSync: true,
  syncIntervalSeconds: 30,
  realtime: true,
  hybridRealtime: true,
  syncAttachments: true,
  syncConfig: false,
  excludePatterns: '',
  conflictStrategy: 'duplicate',
  initialSync: 'merge',
  maxFileSizeMb: 32,
  debug: false,
}

export interface BaselineEntry {
  path: string
  kind: 'file' | 'collection'
  etag?: string
  size: number
  mtime: number
  version: number
  content_type: string
  /**
   * Stable server-side file id (the manifest's `file_id`).
   *
   * It survives a MOVE, which is the only way to tell "the remote renamed this note" apart from
   * "the remote deleted this note and created a different one somewhere else".
   */
  file_id?: string
}

export interface PersistedState {
  deviceId: string
  baseline: Record<string, BaselineEntry>
  lastSyncAt?: number
  lastManifestRevision?: string
  lastRoot?: string
}

export interface PluginData {
  settings: MeiSyncSettings
  state: PersistedState
}

export interface RemoteEntry {
  path: string
  kind: 'file' | 'collection'
  etag?: string
  size: number
  version: number
  content_type: string
  updated_at: string
  /** Server-side file id, stable across renames. */
  file_id?: string
}

export interface RemoteManifest {
  root: string
  vault: string
  revision: string
  generated_at: string
  seq: number
  entries: RemoteEntry[]
}

export interface LocalEntry {
  path: string
  kind: 'file' | 'collection'
  size: number
  mtime: number
}

export interface Handshake {
  service: string
  version: string
  protocol: number
  user: { id: string; username: string; created_at?: string }
  vault: string
  root: string
  home: string
  server_time: string
  seq: number
  max_body_bytes: number
  capabilities: string[]
}

export interface DeviceInfo {
  deviceId: string
  name: string
  platform: string
  pluginVersion: string
}

export interface WriteResult {
  path: string
  etag: string
  size: number
  version: number
  sync_mode: string
  created: boolean
}

export type LogLevel = 'info' | 'warn' | 'error'

export interface LogEntry {
  at: number
  level: LogLevel
  message: string
}
