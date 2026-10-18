import { App, Notice, Platform, TAbstractFile, TFile, TFolder } from 'obsidian'
import { ConflictError, SyncApi, SyncHttpError } from './api'
import type { SyncLog } from './log'
import { plan, type PlanAction, type PlanResult } from './sync/planner'
import {
  conflictPath,
  contentTypeFor,
  isExcluded,
  isTextPath,
  normalizeVaultPath,
  remoteVaultPath,
  websocketUrl,
} from './sync/paths'
import { rebuildBaseline } from './sync/state'
import type {
  BaselineEntry,
  DeviceInfo,
  Handshake,
  LocalEntry,
  MeiSyncSettings,
  PersistedState,
  RemoteEntry,
  RemoteManifest,
} from './types'

export type SyncStatus = 'idle' | 'syncing' | 'offline' | 'error' | 'disabled'

export interface SyncSummary {
  reason: string
  startedAt: number
  durationMs: number
  uploads: number
  downloads: number
  deletesLocal: number
  deletesRemote: number
  mkdirs: number
  conflicts: number
  skipped: number
  errors: number
  ok: boolean
  message?: string
}

export interface SyncEngineOptions {
  app: App
  log: SyncLog
  pluginVersion: string
  getSettings: () => MeiSyncSettings
  getState: () => PersistedState
  onPersist: () => Promise<void>
  onStatus: (status: SyncStatus, detail?: string) => void
  onToken: (token: string) => Promise<void>
  /** Paths currently owned by the hybrid YSync manager; file-level sync must leave them alone. */
  isRealtimePath?: (path: string) => boolean
}

export interface SyncOptions {
  /** Restrict a reconcile to these vault-relative paths (used before opening a YSync session). */
  paths?: Set<string>
}

interface ActionContext {
  vault: string
  summary: SyncSummary
  working: Record<string, BaselineEntry>
  /** Local scan the plan was built from; actions compare against it before they touch a file. */
  planned: Map<string, LocalEntry>
}

const MAX_CONFLICT_COPIES = 4

/**
 * Vault synchronisation engine.
 *
 * The engine is deliberately file-level: Obsidian remains the editor, and every note is still
 * materialised as a normal file by the MeiSync node. Yjs/YSync stays available for clients that
 * want CRDT editing, while an Obsidian vault wants whole-file semantics.
 */
export class SyncEngine {
  private client: SyncApi | null = null
  private running = false
  private stopped = true
  private suspended = false
  private syncing = false
  private pending = false
  private intervalTimer: number | null = null
  private debounceTimer: number | null = null
  private reconnectTimer: number | null = null
  private reconnectAttempt = 0
  private socket: WebSocket | null = null
  private suppressed = new Map<string, number>()
  private skippedPaths = new Set<string>()
  private excludedRemotePaths = new Set<string>()
  private root = ''
  private moveInFlight = new Set<string>()
  /** Paths this round wrote to disk itself; their fresh stat is not a mid-round user edit. */
  private roundWrites = new Set<string>()
  private idleWaiters: Array<() => void> = []
  private wakeCleanup: Array<() => void> = []

  constructor(private readonly options: SyncEngineOptions) {
    this.rebuildClient()
  }

  // -----------------------------------------------------------------------------------------
  // lifecycle
  // -----------------------------------------------------------------------------------------

  start(): void {
    if (this.running) return
    this.running = true
    this.stopped = false
    this.suspended = false
    this.rebuildClient()
    this.restartInterval()
    this.connectRealtime()
    this.registerWakeHandlers()
    if (this.options.getSettings().serverUrl) {
      this.scheduleSync('startup', 1200)
    } else {
      this.setStatus('disabled', 'server URL not configured')
    }
  }

  stop(): void {
    this.running = false
    this.stopped = true
    this.suspended = false
    if (this.intervalTimer !== null) window.clearInterval(this.intervalTimer)
    if (this.debounceTimer !== null) window.clearTimeout(this.debounceTimer)
    this.intervalTimer = null
    this.debounceTimer = null
    this.unregisterWakeHandlers()
    this.disconnectRealtime()
    this.setStatus('disabled')
  }

  /**
   * Catch up when the vault window becomes active again.
   *
   * Browsers throttle timers in background windows and a suspended laptop or a mobile app can
   * leave the realtime socket dead without a `close` event. The moment the user switches back to
   * Obsidian is exactly when they expect the vault to be current, so treat it as a sync trigger
   * (and revive a dead feed) instead of waiting for the next interval tick.
   */
  private registerWakeHandlers(): void {
    if (this.wakeCleanup.length) return
    const wake = (reason: string): void => {
      if (!this.running || this.stopped || this.suspended || !this.isConfigured()) return
      if (typeof WebSocket !== 'undefined' && (!this.socket || this.socket.readyState === WebSocket.CLOSED)) {
        this.connectRealtime()
      }
      if (Date.now() - (this.options.getState().lastSyncAt || 0) < 2000) return
      this.scheduleSync(reason, 300)
    }
    const onFocus = (): void => wake('window focus')
    const onVisibility = (): void => {
      if (typeof document === 'undefined' || document.visibilityState === 'visible') {
        wake('window visible')
      }
    }
    const windowEvents =
      typeof window !== 'undefined' && typeof window.addEventListener === 'function'
    const documentEvents =
      typeof document !== 'undefined' && typeof document.addEventListener === 'function'
    if (windowEvents) {
      window.addEventListener('focus', onFocus)
      this.wakeCleanup.push(() => window.removeEventListener('focus', onFocus))
    }
    if (documentEvents) {
      document.addEventListener('visibilitychange', onVisibility)
      this.wakeCleanup.push(() => document.removeEventListener('visibilitychange', onVisibility))
    }
  }

  private unregisterWakeHandlers(): void {
    for (const remove of this.wakeCleanup) remove()
    this.wakeCleanup = []
  }

  applySettings(): void {
    this.rebuildClient()
    if (!this.running) return
    this.restartInterval()
    // While the user is editing the remote path, do not reconnect the realtime feed on every
    // debounced keystroke; closing the settings tab applies the final value instead.
    if (this.suspended) return
    this.connectRealtime()
    if (this.options.getSettings().serverUrl) this.scheduleSync('settings changed', 400)
  }

  private rebuildClient(): void {
    const settings = this.options.getSettings()
    if (!settings.serverUrl.trim()) {
      this.client = null
      return
    }
    try {
      this.client = new SyncApi(settings.serverUrl, settings.token, this.options.pluginVersion)
    } catch (error) {
      this.client = null
      this.options.log.error(`invalid server URL: ${describeError(error)}`)
    }
  }

  private requireClient(): SyncApi {
    if (!this.client) {
      this.rebuildClient()
      if (!this.client) throw new Error('Set the MeiSync server URL in the plugin settings first.')
    }
    return this.client
  }

  private restartInterval(): void {
    if (this.intervalTimer !== null) window.clearInterval(this.intervalTimer)
    this.intervalTimer = null
    const settings = this.options.getSettings()
    if (!settings.autoSync) return
    const seconds = Math.max(10, Math.min(3600, Number(settings.syncIntervalSeconds) || 30))
    this.intervalTimer = window.setInterval(() => this.scheduleSync('interval'), seconds * 1000)
  }

  private setStatus(status: SyncStatus, detail?: string): void {
    this.options.onStatus(status, detail)
  }

  private persist(): Promise<void> {
    return this.options.onPersist().catch((error) => {
      this.options.log.warn(`could not save sync state: ${describeError(error)}`)
    })
  }

  // -----------------------------------------------------------------------------------------
  // scheduling
  // -----------------------------------------------------------------------------------------

  private isConfigured(): boolean {
    const settings = this.options.getSettings()
    return Boolean(
      settings.serverUrl.trim() &&
        (settings.token || (settings.username.trim() && settings.password)),
    )
  }

  /**
   * Pause background reconciliation while the settings tab is open. Otherwise every keystroke in
   * the “Remote folder” field would be treated as a valid vault root and create a new remote
   * folder (ob/, obsid/, obsidi/, …).
   */
  setSuspended(suspended: boolean): void {
    this.suspended = suspended
    if (!suspended && this.running && !this.stopped) {
      this.scheduleSync('settings applied', 800)
    }
  }

  scheduleSync(reason: string, delay = 1200): void {
    if (!this.running || this.stopped || this.suspended) return
    if (!this.isConfigured()) {
      this.setStatus('disabled', settingsHint(this.options.getSettings()))
      return
    }
    if (this.debounceTimer !== null) window.clearTimeout(this.debounceTimer)
    this.debounceTimer = window.setTimeout(() => {
      this.debounceTimer = null
      // A timer scheduled just before the settings tab opened must not provision a half-typed
      // remote path either.
      if (this.suspended) return
      void this.syncNow(reason).then((summary) => {
        if (!summary.ok && summary.message && reason === 'manual') {
          new Notice(`MeiSync: ${summary.message}`, 8000)
        }
      })
    }, Math.max(100, delay))
  }

  /**
   * Settings "Test connection": verify login, the sync API and device visibility **without**
   * provisioning the remote root. Creating the root is intentionally left to the first real
   * sync, so testing a partially typed remote path cannot create stray folders.
   */
  async testConnection(): Promise<{ user: string; root: string; server: string; devices: number }> {
    this.rebuildClient()
    const client = this.requireClient()
    const settings = this.options.getSettings()
    if (!settings.token) await this.login()
    const info = await client.syncInfo()
    const me = await this.withAuth((current) => current.me())
    const devices = await this.withAuth((current) => current.listDevices())
    const root = `/private/${me.username}/${remoteVaultPath(settings, this.options.app.vault.getName())}`
    return {
      user: me.username,
      root,
      server: `${info.service} ${info.version}`,
      devices: devices.devices.length,
    }
  }

  /** Whether a local rename currently has a remote MOVE in flight. */
  isMoving(path: string): boolean {
    return this.moveInFlight.has(path)
  }

  /** Wait for an in-flight full/targeted reconcile to finish. */
  async whenIdle(): Promise<void> {
    if (!this.syncing) return
    await new Promise<void>((resolve) => this.idleWaiters.push(resolve))
  }

  private async runTargetedSync(path: string, reason: string): Promise<SyncSummary> {
    let summary = emptySummary(reason, 'not started')
    for (let attempt = 0; attempt < 3; attempt += 1) {
      await this.whenIdle()
      summary = await this.syncNow(reason, { paths: new Set([path]) })
      if (summary.ok || summary.message !== 'already syncing') break
    }
    return summary
  }

  /**
   * Reconcile the last editor text of a closing YSync session with the server.
   *
   * If the server already has the same text (normal case), this only refreshes the baseline. If
   * the YSync socket was disconnected while the user was editing, it forces one conditional
   * upload so those final edits cannot be lost by a baseline refresh that would mark them synced.
   */
  async finalizeRealtimePath(path: string, localText: string, allowUpload = true): Promise<void> {
    const normalized = normalizeVaultPath(path)
    const settings = this.options.getSettings()
    const vault = remoteVaultPath(settings, this.options.app.vault.getName())
    try {
      const file = this.options.app.vault.getAbstractFileByPath(normalized)
      if (!(file instanceof TFile)) {
        // The note was renamed or deleted while the close/finalize callback was running. Let the
        // normal file-level MOVE/DELETE path handle it; reconciling this stale path could delete
        // the renamed remote file.
        this.options.log.warn(`skipping YSync finalize for missing local note ${normalized}`)
        this.scheduleSync('realtime finalize missing local', 300)
        return
      }
      const manifest = await this.fetchManifest(vault)
      const remote = manifest.entries.find((entry) => entry.path === normalized)
      if (!remote || remote.kind !== 'file') {
        delete this.options.getState().baseline[normalized]
        await this.persist()
        if (!allowUpload) {
          this.scheduleSync('realtime finalize missing remote', 300)
          return
        }
        await this.runTargetedSync(normalized, 'realtime close')
        return
      }
      let remoteText: string | null = null
      try {
        const result = await this.withAuth((client) => client.getFile(vault, normalized))
        if (result.data) remoteText = new TextDecoder().decode(result.data)
      } catch (error) {
        this.options.log.warn(
          `could not read remote ${normalized} before closing YSync: ${describeError(error)}`,
        )
      }
      await this.whenIdle()
      if (remoteText === localText) {
        this.options.getState().baseline[normalized] = {
          path: normalized,
          kind: 'file',
          etag: remote.etag,
          size: file.stat.size,
          mtime: file.stat.mtime,
          version: remote.version,
          content_type: remote.content_type,
        }
        this.options.getState().lastManifestRevision = manifest.revision
        await this.persist()
        return
      }
      if (!allowUpload) {
        // The YSync socket was not connected/synced when the note closed. Both sides may have
        // changed; leave the stale baseline in place and let the normal file-level conflict
        // handling preserve both copies instead of silently overwriting the remote.
        this.options.log.warn(
          `YSync was offline while editing ${normalized}; leaving reconciliation to the file-level conflict path`,
        )
        this.scheduleSync('realtime conflict', 300)
        return
      }
      // Make the planner see "local changed, remote unchanged" so it uploads the final editor
      // text with If-Match against the observed remote ETag. If the remote changed in the
      // meantime, the normal 412 conflict path runs instead of losing an edit.
      this.options.getState().baseline[normalized] = {
        path: normalized,
        kind: 'file',
        etag: remote.etag,
        size: 0,
        mtime: -1,
        version: remote.version,
        content_type: remote.content_type,
      }
      await this.persist()
      const summary = await this.runTargetedSync(normalized, 'realtime close')
      if (!summary.ok) {
        this.options.log.warn(`final YSync upload failed for ${normalized}: ${summary.message || 'unknown error'}`)
      } else if (summary.skipped > 0) {
        this.options.log.warn(`final YSync upload skipped for ${normalized}; see the sync rules`)
      }
    } catch (error) {
      this.options.log.warn(`could not finalize YSync note ${normalized}: ${describeError(error)}`)
      this.scheduleSync('realtime finalize fallback', 500)
    }
  }

  /**
   * Prepare one note for a YSync session. A targeted file-level reconcile first guarantees that
   * local content and the server YDoc agree; only then is the realtime WebSocket opened.
   */
  async ensureFileForRealtime(path: string): Promise<{ root: string; absolutePath: string }> {
    const normalized = normalizeVaultPath(path)
    if (!normalized) throw new Error('invalid note path')
    if (!this.isConfigured()) {
      throw new Error('MeiSync is not configured or not signed in')
    }
    const summary = await this.runTargetedSync(normalized, 'realtime open')
    if (!summary.ok) {
      throw new Error(summary.message || 'could not reconcile the note before opening YSync')
    }
    if (summary.skipped > 0) {
      throw new Error(`the note was skipped by the sync rules: ${normalized}`)
    }
    if (!this.root) throw new Error('remote root is not available')
    const vault = remoteVaultPath(this.options.getSettings(), this.options.app.vault.getName())
    const manifest = await this.fetchManifest(vault)
    const remote = manifest.entries.find((entry) => entry.path === normalized)
    if (!remote || remote.kind !== 'file') {
      throw new Error(`${normalized} is not a file on the server after reconciliation`)
    }
    return {
      root: this.root,
      absolutePath: `${this.root.replace(/\/$/, '')}/${normalized}`,
    }
  }

  /** Mark an open YSync note as synced after its session closes, without a conflict round. */
  async refreshBaselineForPath(path: string): Promise<void> {
    try {
      const normalized = normalizeVaultPath(path)
      const settings = this.options.getSettings()
      const vault = remoteVaultPath(settings, this.options.app.vault.getName())
      const manifest = await this.fetchManifest(vault)
      const remote = manifest.entries.find((entry) => entry.path === normalized)
      const file = this.options.app.vault.getAbstractFileByPath(normalized)
      const state = this.options.getState()
      if (remote && remote.kind === 'file' && file instanceof TFile) {
        state.baseline[normalized] = {
          path: normalized,
          kind: 'file',
          etag: remote.etag,
          size: file.stat.size,
          mtime: file.stat.mtime,
          version: remote.version,
          content_type: remote.content_type,
        }
        state.lastManifestRevision = manifest.revision
      } else if (!remote) {
        delete state.baseline[normalized]
      }
      await this.persist()
    } catch (error) {
      this.options.log.warn(`could not refresh the YSync baseline for ${path}: ${describeError(error)}`)
    }
  }

  async syncNow(reason = 'manual', options: SyncOptions = {}): Promise<SyncSummary> {
    if (this.syncing) {
      this.pending = true
      if (reason === 'manual') new Notice('MeiSync: a sync is already running')
      return emptySummary(reason, 'already syncing')
    }
    if (this.stopped) return emptySummary(reason, 'plugin stopped')
    // Settings may have changed since the last sync; rebuild from the current values so a manual
    // sync right after editing the server URL uses the new endpoint.
    this.rebuildClient()
    const startedAt = Date.now()
    const summary: SyncSummary = {
      reason,
      startedAt,
      durationMs: 0,
      uploads: 0,
      downloads: 0,
      deletesLocal: 0,
      deletesRemote: 0,
      mkdirs: 0,
      conflicts: 0,
      skipped: 0,
      errors: 0,
      ok: true,
    }
    this.syncing = true
    this.skippedPaths = new Set()
    this.excludedRemotePaths = new Set()
    this.roundWrites = new Set()
    this.setStatus('syncing', reason)
    this.options.log.info(`sync started (${reason})`)

    const settings = this.options.getSettings()
    let working: Record<string, BaselineEntry> = Object.assign(
      Object.create(null) as Record<string, BaselineEntry>,
      this.options.getState().baseline,
    )

    try {
      const handshake = await this.ensureSession()
      this.root = handshake.root
      const vault = remoteVaultPath(settings, this.options.app.vault.getName())
      this.applyRoot(handshake)
      working = this.filteredBaseline(settings)

      const targets = options.paths
      const skipForRealtime = (path: string): boolean => {
        if (targets) return !targets.has(path)
        return this.options.isRealtimePath?.(path) === true
      }
      for (const path of Object.keys(working)) {
        if (skipForRealtime(path)) delete working[path]
      }

      const manifest = await this.fetchManifest(vault)
      const local = await this.scanLocal(settings)
      const remote = this.mapRemote(manifest, settings)
      for (const path of [...local.keys()]) {
        if (skipForRealtime(path)) local.delete(path)
      }
      for (const path of [...remote.keys()]) {
        if (skipForRealtime(path)) remote.delete(path)
      }
      const result = plan({
        baseline: working,
        local,
        remote,
        conflictStrategy: settings.conflictStrategy,
        initialSync: settings.initialSync,
      })
      const context: ActionContext = { vault, summary, working, planned: local }
      const actions = this.filterSkippedActions(result.actions, summary)
      this.options.log.info(
        `plan: ${actions.length} action(s), ${local.size} local, ${remote.size} remote, ${Object.keys(working).length} baseline`,
      )
      if (settings.debug && actions.length) {
        for (const action of actions) this.options.log.info(`  ${describeAction(action)}`)
      }

      for (const action of actions) {
        if (this.stopped) {
          this.options.log.warn('sync interrupted by plugin unload')
          break
        }
        if ('path' in action && action.path && this.pathIsSkipped(action.path)) {
          summary.skipped += 1
          continue
        }
        if ('path' in action && action.path && skipForRealtime(action.path)) {
          summary.skipped += 1
          continue
        }
        try {
          await this.execute(action, context)
        } catch (error) {
          summary.errors += 1
          if ('path' in action && action.path) this.skippedPaths.add(action.path)
          this.options.log.error(`${describeAction(action)} failed: ${describeError(error)}`)
          if (isFatal(error)) break
        }
      }

      await this.finishBaseline(settings, vault, working, local, summary, targets)
      summary.ok = summary.errors === 0
      if (summary.conflicts > 0) {
        this.options.log.warn(`${summary.conflicts} conflict(s) were resolved according to “${settings.conflictStrategy}”`)
      }
      if (summary.errors > 0) {
        summary.message = `${summary.errors} action(s) failed; see the sync log`
        this.setStatus('error', summary.message)
      } else {
        this.setStatus('idle')
      }
      this.options.log.info(
        `sync finished in ${Date.now() - startedAt}ms: ${summary.uploads} up, ${summary.downloads} down, ` +
          `${summary.deletesLocal + summary.deletesRemote} deleted, ${summary.mkdirs} folders, ` +
          `${summary.conflicts} conflicts, ${summary.skipped} skipped`,
      )
      if (reason === 'manual' || summary.conflicts > 0 || summary.errors > 0) {
        new Notice(
          `MeiSync: ${summary.uploads} uploaded, ${summary.downloads} downloaded, ` +
            `${summary.conflicts} conflict(s), ${summary.errors} error(s)`,
          summary.errors ? 8000 : 4000,
        )
      }
    } catch (error) {
      summary.errors = Math.max(1, summary.errors)
      summary.ok = false
      summary.message = describeError(error)
      this.options.log.error(`sync failed: ${summary.message}`)
      this.setStatus('error', summary.message)
      if (reason === 'manual') new Notice(`MeiSync: ${summary.message}`, 9000)
    } finally {
      summary.durationMs = Date.now() - startedAt
      this.syncing = false
      this.options.getState().lastSyncAt = Date.now()
      await this.persist()
      if (this.pending) {
        this.pending = false
        this.scheduleSync('queued', 500)
      }
      const waiters = this.idleWaiters
      this.idleWaiters = []
      for (const resolve of waiters) resolve()
    }
    return summary
  }

  // -----------------------------------------------------------------------------------------
  // auth / handshake
  // -----------------------------------------------------------------------------------------

  async login(username = this.options.getSettings().username, password = this.options.getSettings().password): Promise<void> {
    const client = this.requireClient()
    if (!username || !password) {
      throw new Error('MeiSync is not signed in. Enter a username and password in the plugin settings.')
    }
    const result = await client.login(username, password)
    client.setToken(result.token)
    this.options.getSettings().token = result.token
    await this.options.onToken(result.token)
    this.options.log.info(`signed in as ${result.user.username}`)
    // The realtime feed needs a token; reconnect now that one is available. While the settings
    // tab is open we deliberately keep the old connection state: the WebSocket handshake also
    // provisions the remote root, so it must not run for a half-typed path.
    if (this.running && !this.suspended && this.options.getSettings().realtime) {
      this.connectRealtime()
    }
  }

  private async ensureSession(): Promise<Handshake> {
    const client = this.requireClient()
    const settings = this.options.getSettings()
    const vault = remoteVaultPath(settings, this.options.app.vault.getName())
    const device = this.deviceInfo()
    if (!settings.token) await this.login()
    try {
      return await client.handshake(vault, device)
    } catch (error) {
      if (error instanceof SyncHttpError && error.status === 401) {
        this.options.log.warn('session expired; signing in again')
        await this.login()
        return await this.requireClient().handshake(vault, device)
      }
      throw error
    }
  }

  private async withAuth<T>(operation: (client: SyncApi) => Promise<T>): Promise<T> {
    const client = this.requireClient()
    try {
      return await operation(client)
    } catch (error) {
      if (error instanceof SyncHttpError && error.status === 401) {
        await this.login()
        return await operation(this.requireClient())
      }
      throw error
    }
  }

  private deviceInfo(): DeviceInfo {
    const settings = this.options.getSettings()
    const platform = Platform.isMacOS
      ? 'macos'
      : Platform.isWin
        ? 'windows'
        : Platform.isLinux
          ? 'linux'
          : Platform.isMobile
            ? 'mobile'
            : 'desktop'
    return {
      deviceId: this.options.getState().deviceId,
      name: settings.deviceName.trim() || `${platform} · ${this.options.app.vault.getName()}`,
      platform,
      pluginVersion: this.options.pluginVersion,
    }
  }

  private applyRoot(handshake: Handshake): void {
    const state = this.options.getState()
    if (state.lastRoot && state.lastRoot !== handshake.root) {
      this.options.log.warn(
        `remote root changed from ${state.lastRoot} to ${handshake.root}; resetting the sync baseline`,
      )
      state.baseline = {}
      state.lastManifestRevision = undefined
    }
    state.lastRoot = handshake.root
  }

  // -----------------------------------------------------------------------------------------
  // manifests / local scan
  // -----------------------------------------------------------------------------------------

  private async fetchManifest(vault: string): Promise<RemoteManifest> {
    return this.withAuth(async (client) => {
      const result = await client.manifest(vault)
      if (!result.data) throw new Error('server returned an empty manifest')
      return result.data
    })
  }

  private mapRemote(manifest: RemoteManifest, settings: MeiSyncSettings): Map<string, RemoteEntry> {
    const remote = new Map<string, RemoteEntry>()
    const maxBytes = Math.max(1, Number(settings.maxFileSizeMb) || 32) * 1024 * 1024
    for (const entry of manifest.entries) {
      let path: string
      try {
        path = normalizeVaultPath(entry.path)
      } catch {
        this.options.log.warn(`ignoring invalid remote path: ${entry.path}`)
        continue
      }
      if (!path) continue
      if (isExcluded(path, settings)) {
        this.excludedRemotePaths.add(path)
        continue
      }
      const mapped: RemoteEntry = {
        path,
        kind: entry.kind === 'collection' ? 'collection' : 'file',
        etag: entry.etag,
        size: entry.size,
        version: entry.version,
        content_type: entry.content_type,
        updated_at: entry.updated_at,
      }
      if (mapped.kind === 'file' && mapped.size > maxBytes) {
        this.skippedPaths.add(path)
        this.options.log.warn(
          `skipping ${path}: ${formatBytes(mapped.size)} exceeds the ${settings.maxFileSizeMb} MB client limit`,
        )
      }
      remote.set(path, mapped)
    }
    return remote
  }

  private async scanLocal(settings: MeiSyncSettings): Promise<Map<string, LocalEntry>> {
    const local = new Map<string, LocalEntry>()
    const maxBytes = Math.max(1, Number(settings.maxFileSizeMb) || 32) * 1024 * 1024
    const vault = this.options.app.vault
    for (const abstract of vault.getAllLoadedFiles()) {
      let path: string
      try {
        path = normalizeVaultPath(abstract.path)
      } catch {
        this.options.log.warn(`ignoring invalid local path: ${abstract.path}`)
        continue
      }
      if (!path || isExcluded(path, settings)) continue
      if (abstract instanceof TFolder) {
        local.set(path, { path, kind: 'collection', size: 0, mtime: 0 })
        continue
      }
      if (abstract instanceof TFile) {
        if (!settings.syncAttachments && !isTextPath(path)) continue
        if (abstract.stat.size > maxBytes) {
          this.skippedPaths.add(path)
          this.options.log.warn(
            `skipping ${path}: ${formatBytes(abstract.stat.size)} exceeds the ${settings.maxFileSizeMb} MB client limit`,
          )
        }
        local.set(path, {
          path,
          kind: 'file',
          size: abstract.stat.size,
          mtime: abstract.stat.mtime,
        })
      }
    }
    // `.obsidian` files are not always part of the vault index. When configuration sync is on,
    // walk the hidden directory through the adapter as well.
    if (settings.syncConfig && !isExcluded('.obsidian', settings)) {
      await this.scanHiddenConfig(local, settings, maxBytes)
    }
    return local
  }

  private async scanHiddenConfig(
    local: Map<string, LocalEntry>,
    settings: MeiSyncSettings,
    maxBytes: number,
  ): Promise<void> {
    const adapter = this.options.app.vault.adapter
    const stack = ['.obsidian']
    while (stack.length) {
      const folder = stack.pop()
      if (!folder) continue
      if (isExcluded(folder, settings)) continue
      if (!local.has(folder)) {
        local.set(folder, { path: folder, kind: 'collection', size: 0, mtime: 0 })
      }
      let listed: { files: string[]; folders: string[] }
      try {
        listed = await adapter.list(folder)
      } catch {
        continue
      }
      for (const child of listed.folders) {
        const path = normalizeVaultPath(child.startsWith(`${folder}/`) ? child : `${folder}/${child}`)
        if (!isExcluded(path, settings)) stack.push(path)
      }
      for (const child of listed.files) {
        const path = normalizeVaultPath(child.startsWith(`${folder}/`) ? child : `${folder}/${child}`)
        if (isExcluded(path, settings)) continue
        if (!settings.syncAttachments && !isTextPath(path)) continue
        const stat = await adapter.stat(path).catch(() => null)
        if (!stat || stat.type !== 'file') continue
        if (stat.size > maxBytes) {
          this.skippedPaths.add(path)
          continue
        }
        local.set(path, { path, kind: 'file', size: stat.size, mtime: stat.mtime })
      }
    }
  }

  private filteredBaseline(settings: MeiSyncSettings): Record<string, BaselineEntry> {
    const baseline: Record<string, BaselineEntry> = Object.create(null) as Record<string, BaselineEntry>
    for (const [path, entry] of Object.entries(this.options.getState().baseline)) {
      let normalized: string
      try {
        normalized = normalizeVaultPath(path)
      } catch {
        continue
      }
      if (!normalized || isExcluded(normalized, settings)) continue
      baseline[normalized] = entry
    }
    return baseline
  }

  private filterSkippedActions(actions: PlanAction[], summary: SyncSummary): PlanAction[] {
    if (!this.skippedPaths.size) return actions
    return actions.filter((action) => {
      const path = 'path' in action ? action.path : ''
      if (path && this.pathIsSkipped(path)) {
        summary.skipped += 1
        return false
      }
      return true
    })
  }

  /** A skipped folder also skips its children: an unresolved folder/file collision must not
   * download into the local file that occupies the folder path. */
  private pathIsSkipped(path: string): boolean {
    if (this.skippedPaths.has(path)) return true
    const parts = path.split('/')
    let prefix = ''
    for (const part of parts.slice(0, -1)) {
      prefix = prefix ? `${prefix}/${part}` : part
      if (this.skippedPaths.has(prefix)) return true
    }
    return false
  }

  private hasSkippedDescendant(path: string): boolean {
    const prefix = `${path}/`
    for (const skipped of this.skippedPaths) {
      if (skipped.startsWith(prefix)) return true
    }
    return false
  }

  private hasExcludedRemoteDescendant(path: string): boolean {
    const prefix = `${path}/`
    for (const excluded of this.excludedRemotePaths) {
      if (excluded === path || excluded.startsWith(prefix)) return true
    }
    return false
  }

  // -----------------------------------------------------------------------------------------
  // action execution
  // -----------------------------------------------------------------------------------------

  private async execute(action: PlanAction, context: ActionContext): Promise<void> {
    switch (action.type) {
      case 'mkdir-local':
        await this.ensureFolder(action.path)
        context.summary.mkdirs += 1
        context.working[action.path] = collectionBaseline(action.path)
        return
      case 'mkdir-remote':
        await this.withAuth((client) => client.mkdir(context.vault, action.path))
        context.summary.mkdirs += 1
        context.working[action.path] = collectionBaseline(action.path)
        return
      case 'download':
        await this.download(action.path, action.remote, context)
        return
      case 'upload':
        await this.upload(action.path, action.local, action.baseEtag, action.force, context)
        return
      case 'delete-local':
        if (this.hasSkippedDescendant(action.path)) {
          this.options.log.warn(`not deleting ${action.path}: a child could not be synced`)
          this.skippedPaths.add(action.path)
          return
        }
        await this.deleteLocal(action.path, action.kind)
        context.summary.deletesLocal += 1
        delete context.working[action.path]
        return
      case 'delete-remote':
        if (this.hasSkippedDescendant(action.path)) {
          this.options.log.warn(`not deleting remote ${action.path}: a child could not be synced`)
          this.skippedPaths.add(action.path)
          return
        }
        if (action.kind === 'collection' && this.hasExcludedRemoteDescendant(action.path)) {
          this.options.log.warn(
            `not deleting remote folder ${action.path}: it contains excluded or unsupported files`,
          )
          this.skippedPaths.add(action.path)
          return
        }
        await this.withAuth((client) =>
          client.deleteFile(context.vault, action.path, { etag: action.etag, force: action.force }),
        )
        context.summary.deletesRemote += 1
        delete context.working[action.path]
        return
      case 'forget-baseline':
        delete context.working[action.path]
        return
      case 'conflict':
        await this.resolveConflict(action, context)
        return
    }
  }

  private async download(path: string, remote: RemoteEntry, context: ActionContext): Promise<void> {
    const result = await this.withAuth((client) => client.getFile(context.vault, path))
    if (!result.notModified && result.data) {
      // The plan was built from an earlier scan. An edit that landed while the round was running
      // would be overwritten here without a trace, so keep it as a conflict copy first.
      await this.preserveMidRoundEdit(path, context)
      await this.writeLocal(path, result.data)
      context.summary.downloads += 1
    }
    const stat = await this.localStat(path)
    context.working[path] = {
      path,
      kind: 'file',
      etag: result.etag || remote.etag,
      size: stat?.size ?? remote.size,
      mtime: stat?.mtime ?? Date.now(),
      version: remote.version,
      content_type: remote.content_type,
    }
  }

  /**
   * Save the current content of a file that changed after the plan was computed as a conflict copy.
   *
   * Downloading over such a file would silently destroy an edit the user made while the round was
   * running, and the baseline would then record the remote content as synced. Reuse the normal
   * conflict-copy path so both versions survive.
   */
  private async preserveMidRoundEdit(path: string, context: ActionContext): Promise<void> {
    const planned = context.planned.get(path)
    if (!planned || planned.kind !== 'file') return
    const stat = await this.localStat(path)
    if (!stat) return
    if (stat.size === planned.size && Math.abs(stat.mtime - planned.mtime) <= 1) return
    const localData = await this.readLocal(path).catch(() => null)
    if (!localData) return
    await this.storeConflictCopy(path, localData, context)
    context.summary.conflicts += 1
  }

  private async upload(
    path: string,
    local: LocalEntry,
    baseEtag: string | null,
    force: boolean,
    context: ActionContext,
  ): Promise<void> {
    const data = await this.readLocal(path)
    try {
      const result = await this.withAuth((client) =>
        client.putFile(context.vault, path, data, {
          baseEtag,
          force,
          contentType: contentTypeFor(path),
        }),
      )
      context.summary.uploads += 1
      const stat = await this.localStat(path)
      context.working[path] = {
        path,
        kind: 'file',
        etag: result.etag,
        size: stat?.size ?? local.size,
        mtime: stat?.mtime ?? local.mtime,
        version: result.version,
        content_type: contentTypeFor(path),
      }
    } catch (error) {
      if (error instanceof ConflictError) {
        this.options.log.warn(`${path} changed remotely while uploading; resolving conflict`)
        await this.resolveUploadConflict(path, data, context)
        return
      }
      throw error
    }
  }

  private async deleteLocal(path: string, kind: 'file' | 'collection'): Promise<void> {
    const target = this.options.app.vault.getAbstractFileByPath(path)
    if (!target) {
      // Hidden / unindexed files still live in the adapter.
      try {
        if (kind === 'collection') await this.options.app.vault.adapter.rmdir(path, true)
        else await this.options.app.vault.adapter.remove(path)
      } catch {
        /* already gone */
      }
      return
    }
    if (kind === 'collection' && target instanceof TFolder) {
      if (target.children.length > 0) {
        this.options.log.warn(`not deleting ${path}: the local folder is not empty`)
        this.skippedPaths.add(path)
        return
      }
      // Indexed children may be empty while hidden / excluded files still exist on disk.
      const listed = await this.options.app.vault.adapter.list(path).catch(() => null)
      if (listed && (listed.files.length > 0 || listed.folders.length > 0)) {
        this.options.log.warn(
          `not deleting ${path}: the folder still contains excluded or unsynced entries`,
        )
        this.skippedPaths.add(path)
        return
      }
    }
    this.suppress(path)
    try {
      await this.options.app.vault.trash(target, true)
    } catch {
      await this.options.app.vault.delete(target)
    }
  }

  private async resolveUploadConflict(path: string, localData: ArrayBuffer, context: ActionContext): Promise<void> {
    const remote = await this.withAuth((client) => client.getFile(context.vault, path))
    if (!remote.data) throw new ConflictError(`${path}: remote content disappeared during conflict resolution`)
    if (await sameBytes(localData, remote.data)) {
      this.options.log.info(`${path} already matches the remote content; conflict cleared`)
      const stat = await this.localStat(path)
      context.working[path] = {
        path,
        kind: 'file',
        etag: remote.etag,
        size: stat?.size ?? localData.byteLength,
        mtime: stat?.mtime ?? Date.now(),
        version: 0,
        content_type: contentTypeFor(path),
      }
      return
    }
    await this.storeConflictCopy(path, localData, context)
    await this.writeLocal(path, remote.data)
    // Record the state we just wrote, so the baseline refresh does not mistake it for a local edit
    // that landed during the round.
    const stat = await this.localStat(path)
    context.working[path] = {
      path,
      kind: 'file',
      etag: remote.etag,
      size: stat?.size ?? remote.data.byteLength,
      mtime: stat?.mtime ?? Date.now(),
      version: 0,
      content_type: contentTypeFor(path),
    }
    context.summary.conflicts += 1
  }

  private async resolveConflict(
    action: Extract<PlanAction, { type: 'conflict' }>,
    context: ActionContext,
  ): Promise<void> {
    const { path, local, remote, conflict } = action
    const settings = this.options.getSettings()
    this.options.log.warn(`conflict at ${path}: ${conflict}`)

    if (local?.kind === 'collection' || remote?.kind === 'collection') {
      this.options.log.error(
        `cannot merge a file/folder collision at ${path}; move or rename one side manually`,
      )
      this.skippedPaths.add(path)
      context.summary.conflicts += 1
      return
    }

    if (local && remote && local.kind === 'file') {
      const localData = await this.readLocal(path).catch(() => null)
      const remoteResult = await this.withAuth((client) => client.getFile(context.vault, path))
      if (localData && remoteResult.data && (await sameBytes(localData, remoteResult.data))) {
        this.options.log.info(`${path} is identical on both sides; conflict cleared`)
        const stat = await this.localStat(path)
        context.working[path] = {
          path,
          kind: 'file',
          etag: remoteResult.etag,
          size: stat?.size ?? local.size,
          mtime: stat?.mtime ?? Date.now(),
          version: remote.version,
          content_type: remote.content_type,
        }
        return
      }
      if (localData) await this.storeConflictCopy(path, localData, context)
      if (remoteResult.data) {
        await this.writeLocal(path, remoteResult.data)
        // Record the state we just wrote, so the baseline refresh does not mistake it for a local
        // edit that landed during the round.
        const stat = await this.localStat(path)
        context.working[path] = {
          path,
          kind: 'file',
          etag: remoteResult.etag ?? remote.etag,
          size: stat?.size ?? remoteResult.data.byteLength,
          mtime: stat?.mtime ?? Date.now(),
          version: remote.version,
          content_type: remote.content_type,
        }
      }
      context.summary.conflicts += 1
      return
    }

    if (local && !remote) {
      // Local edit versus remote deletion: resurrecting the local file is less destructive than
      // deleting the last edited copy.
      await this.upload(path, local, null, true, context)
      context.summary.conflicts += 1
      this.options.log.warn(`${path} was deleted remotely but edited locally; local version restored`)
      return
    }

    if (!local && remote) {
      // Local deletion versus remote edit: restore the remote file.
      await this.download(path, remote, context)
      context.summary.conflicts += 1
      this.options.log.warn(`${path} was deleted locally but changed remotely; remote version restored`)
      return
    }

    this.options.log.warn(`nothing to resolve for ${path}`)
    this.skippedPaths.add(path)
    if (settings.conflictStrategy === 'manual') {
      this.options.log.info(`manual strategy: review ${path} and sync again`)
    }
  }

  private async storeConflictCopy(
    path: string,
    data: ArrayBuffer,
    context: ActionContext,
  ): Promise<string> {
    const settings = this.options.getSettings()
    const device = this.deviceInfo().name
    let copy = conflictPath(path, device)
    for (let attempt = 0; attempt < MAX_CONFLICT_COPIES; attempt += 1) {
      const exists = await this.options.app.vault.adapter.exists(copy)
      if (!exists) break
      copy = conflictPath(path, `${device}-${attempt + 2}`)
    }
    await this.writeLocal(copy, data, false)
    this.options.log.warn(`saved local conflict copy: ${copy}`)
    if (settings.conflictStrategy !== 'manual') {
      // Only the conflict copy is uploaded; the original path keeps the remote content.
      await this.withAuth((client) =>
        client.putFile(context.vault, copy, data, {
          force: false,
          contentType: contentTypeFor(copy),
        }),
      )
      const stat = await this.localStat(copy)
      context.working[copy] = {
        path: copy,
        kind: 'file',
        etag: undefined,
        size: stat?.size ?? data.byteLength,
        mtime: stat?.mtime ?? Date.now(),
        version: 0,
        content_type: contentTypeFor(copy),
      }
      context.summary.uploads += 1
    }
    return copy
  }

  private async finishBaseline(
    settings: MeiSyncSettings,
    vault: string,
    working: Record<string, BaselineEntry>,
    planned: Map<string, LocalEntry>,
    summary: SyncSummary,
    targets?: Set<string>,
  ): Promise<void> {
    try {
      const manifest = await this.fetchManifest(vault)
      const local = await this.scanLocal(settings)
      const remote = this.mapRemote(manifest, settings)
      const skipForRealtime = (path: string): boolean => {
        if (targets) return !targets.has(path)
        return this.options.isRealtimePath?.(path) === true
      }
      for (const path of [...local.keys()]) {
        if (skipForRealtime(path)) local.delete(path)
      }
      for (const path of [...remote.keys()]) {
        if (skipForRealtime(path)) remote.delete(path)
      }
      // The local stat each path is expected to have now: the plan-time scan, corrected for the
      // files this round wrote itself (a download is not a mid-round user edit).
      const expected = new Map(planned)
      for (const path of this.roundWrites) {
        const entry = working[path]
        if (entry && entry.kind === 'file') {
          expected.set(path, { path, kind: 'file', size: entry.size, mtime: entry.mtime })
        }
      }
      const rebuilt = rebuildBaseline(local, remote, working, this.skippedPaths, expected)
      const state = this.options.getState()
      if (targets) {
        // A targeted reconcile only updates its own paths; leave every other baseline entry alone.
        const next = Object.assign(Object.create(null) as Record<string, BaselineEntry>, state.baseline)
        for (const path of targets) {
          if (rebuilt[path]) next[path] = rebuilt[path]
          else delete next[path]
        }
        state.baseline = next
      } else {
        // Open YSync notes are intentionally excluded from file-level sync. Preserve their old
        // baseline until the session closes and refreshBaselineForPath runs.
        const next = Object.assign(Object.create(null) as Record<string, BaselineEntry>, rebuilt)
        for (const [path, entry] of Object.entries(state.baseline)) {
          if (this.options.isRealtimePath?.(path) && !(path in next)) next[path] = entry
        }
        state.baseline = next
      }
      state.lastManifestRevision = manifest.revision
      state.lastRoot = this.root
    } catch (error) {
      this.options.log.warn(
        `could not refresh the baseline after sync; keeping incremental state: ${describeError(error)}`,
      )
      const state = this.options.getState()
      if (targets) {
        const next = Object.assign(Object.create(null) as Record<string, BaselineEntry>, state.baseline)
        for (const path of targets) {
          if (working[path]) next[path] = working[path]
          else delete next[path]
        }
        state.baseline = next
      } else {
        const next = Object.assign(Object.create(null) as Record<string, BaselineEntry>, working)
        for (const [path, entry] of Object.entries(state.baseline)) {
          if (this.options.isRealtimePath?.(path) && !(path in next)) next[path] = entry
        }
        state.baseline = next
      }
      summary.errors += 1
    }
  }

  // -----------------------------------------------------------------------------------------
  // local file operations
  // -----------------------------------------------------------------------------------------

  private async readLocal(path: string): Promise<ArrayBuffer> {
    const file = this.options.app.vault.getAbstractFileByPath(path)
    if (file instanceof TFile) return this.options.app.vault.readBinary(file)
    // Hidden `.obsidian` files are not always part of the vault index; the adapter can still
    // read them.
    if (await this.options.app.vault.adapter.exists(path)) {
      return this.options.app.vault.adapter.readBinary(path)
    }
    throw new Error(`${path} is not a local file`)
  }

  private async writeLocal(path: string, data: ArrayBuffer, suppress = true): Promise<void> {
    const parent = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''
    await this.ensureFolder(parent)
    const existing = this.options.app.vault.getAbstractFileByPath(path)
    let text: string | null = null
    if (isTextPath(path)) {
      try {
        text = new TextDecoder('utf-8', { fatal: true }).decode(data)
      } catch {
        // Not valid UTF-8 despite the extension (a mislabelled binary); fall back to a binary
        // write instead of corrupting it with replacement characters.
        text = null
      }
    }
    if (existing instanceof TFile) {
      if (text !== null) await this.options.app.vault.modify(existing, text)
      else await this.options.app.vault.modifyBinary(existing, data)
    } else if (existing) {
      throw new Error(`${path} exists and is not a file`)
    } else if (text !== null && !path.startsWith('.obsidian/')) {
      await this.options.app.vault.create(path, text)
    } else if (text !== null) {
      await this.options.app.vault.adapter.write(path, text)
    } else if (!path.startsWith('.obsidian/')) {
      await this.options.app.vault.createBinary(path, data)
    } else {
      await this.options.app.vault.adapter.writeBinary(path, data)
    }
    if (suppress) this.suppress(path)
    this.roundWrites.add(path)
  }

  private async ensureFolder(path: string): Promise<void> {
    const normalized = normalizeVaultPath(path)
    if (!normalized) return
    let current = ''
    for (const segment of normalized.split('/')) {
      current = current ? `${current}/${segment}` : segment
      const existing = this.options.app.vault.getAbstractFileByPath(current)
      if (existing instanceof TFolder) continue
      if (existing) throw new Error(`${current} exists and is not a folder`)
      const stat = await this.options.app.vault.adapter.stat(current).catch(() => null)
      if (stat?.type === 'folder') continue
      if (stat?.type === 'file') throw new Error(`${current} exists and is not a folder`)
      try {
        await this.options.app.vault.createFolder(current)
      } catch (error) {
        // A concurrent creation (another plugin, another sync) is fine as long as a folder is
        // there now. Hidden config directories may not be indexable through the Vault API.
        const check = this.options.app.vault.getAbstractFileByPath(current)
        if (!(check instanceof TFolder)) {
          try {
            await this.options.app.vault.adapter.mkdir(current)
          } catch {
            throw error
          }
        }
      }
    }
  }

  private async localStat(path: string): Promise<{ mtime: number; size: number } | null> {
    try {
      const stat = await this.options.app.vault.adapter.stat(path)
      if (!stat || stat.type !== 'file') return null
      return { mtime: stat.mtime, size: stat.size }
    } catch {
      return null
    }
  }

  private suppress(path: string, ms = 2500): void {
    this.suppressed.set(path, Date.now() + ms)
  }

  private isSuppressed(path: string): boolean {
    const until = this.suppressed.get(path)
    if (until === undefined) return false
    if (Date.now() > until) {
      this.suppressed.delete(path)
      return false
    }
    return true
  }

  // -----------------------------------------------------------------------------------------
  // local event handling / rename optimisation
  // -----------------------------------------------------------------------------------------

  handleLocalChange(path: string): void {
    if (!this.running) return
    let normalized: string
    try {
      normalized = normalizeVaultPath(path)
    } catch {
      return
    }
    if (!normalized || this.isSuppressed(normalized)) return
    if (this.options.isRealtimePath?.(normalized)) return
    if (isExcluded(normalized, this.options.getSettings())) return
    this.scheduleSync(`local change: ${normalized}`, 1400)
  }

  handleLocalDelete(path: string): void {
    if (!this.running) return
    let normalized: string
    try {
      normalized = normalizeVaultPath(path)
    } catch {
      return
    }
    if (!normalized || this.isSuppressed(normalized)) return
    if (this.options.isRealtimePath?.(normalized)) return
    if (isExcluded(normalized, this.options.getSettings())) return
    this.scheduleSync(`local delete: ${normalized}`, 700)
  }

  handleLocalRename(file: TAbstractFile, oldPath: string): void {
    if (!this.running) return
    let from: string
    let to: string
    try {
      from = normalizeVaultPath(oldPath)
      to = normalizeVaultPath(file.path)
    } catch {
      this.scheduleSync('local rename', 500)
      return
    }
    if (!from || !to || from === to) return
    if (isExcluded(from, this.options.getSettings()) || isExcluded(to, this.options.getSettings())) {
      this.scheduleSync('local rename across exclude boundary', 500)
      return
    }
    if (this.moveInFlight.has(from)) return
    if (this.syncing) {
      // The running round planned against the pre-rename tree: it may still write the old path,
      // and its `finishBaseline` is built from a manifest fetched before the MOVE. Keep the old
      // path out of both until the MOVE below has remapped it.
      this.skippedPaths.add(from)
    }
    void this.tryRemoteMove(from, to, file instanceof TFolder)
  }

  private async tryRemoteMove(from: string, to: string, isFolder: boolean): Promise<void> {
    const state = this.options.getState()
    const prefix = `${from}/`
    // Snapshot the entries the MOVE has to remap *before* any await: a round that is already
    // running finishes with a baseline rebuilt from a pre-MOVE manifest, which drops the old path
    // (the local scan only has the new one) and would otherwise lose the remap for good.
    const moved = Object.entries(state.baseline).filter(
      ([path]) => path === from || path.startsWith(prefix),
    )
    if (!moved.length) {
      // Never synced (or already reset): a full reconcile is the safe path.
      this.scheduleSync('rename without baseline', 500)
      return
    }
    this.moveInFlight.add(from)
    try {
      // Run the MOVE after the current round instead of racing it: a concurrent MOVE would let the
      // round upload or re-download the old path, and the round's baseline refresh would then
      // disagree with the remap below.
      await this.whenIdle()
      const vault = remoteVaultPath(this.options.getSettings(), this.options.app.vault.getName())
      await this.withAuth((client) => client.move(vault, from, to, false))
      // Re-apply the snapshot on top of whatever the baseline holds now, so the rename survives
      // even when the round dropped the old path's entry. An entry the round did update wins over
      // the snapshot, because it carries the ETag that round produced.
      const remapped: Record<string, BaselineEntry> = Object.create(null) as Record<string, BaselineEntry>
      for (const [path, entry] of Object.entries(state.baseline)) {
        if (path === from || path.startsWith(prefix)) continue
        remapped[path] = entry
      }
      for (const [path, entry] of moved) {
        const suffix = path === from ? '' : path.slice(from.length)
        const next = `${to}${suffix}`
        remapped[next] = { ...(state.baseline[path] ?? entry), path: next }
      }
      state.baseline = remapped
      await this.persist()
      this.options.log.info(`remote move: ${from} -> ${to}${isFolder ? ' (folder)' : ''}`)
    } catch (error) {
      if (error instanceof SyncHttpError && (error.status === 409 || error.status === 404)) {
        this.options.log.warn(`remote move failed (${error.status}); falling back to a full reconcile`)
        this.scheduleSync('rename fallback', 500)
      } else {
        this.options.log.error(`remote move failed: ${describeError(error)}`)
        this.scheduleSync('rename retry', 3000)
      }
    } finally {
      this.moveInFlight.delete(from)
    }
  }

  // -----------------------------------------------------------------------------------------
  // realtime feed
  // -----------------------------------------------------------------------------------------

  private connectRealtime(): void {
    if (this.suspended) return
    this.disconnectRealtime()
    const settings = this.options.getSettings()
    if (!this.running || !settings.realtime || !settings.token || !settings.serverUrl) return
    if (typeof WebSocket === 'undefined') {
      this.options.log.warn('WebSocket is not available in this environment; using polling only')
      return
    }
    let url: string
    try {
      const vault = remoteVaultPath(settings, this.options.app.vault.getName())
      url = websocketUrl(settings.serverUrl, vault, settings.token)
    } catch (error) {
      this.options.log.warn(`cannot open the realtime feed: ${describeError(error)}`)
      return
    }
    try {
      const socket = new WebSocket(url)
      this.socket = socket
      socket.onopen = () => {
        this.reconnectAttempt = 0
        this.options.log.info('realtime feed connected')
        if (!this.syncing) this.setStatus('idle')
        // A (re)connect can follow a gap in which changes were broadcast to nobody. Always
        // reconcile once, regardless of how recently the last round finished, so a quick
        // reconnect cannot leave the vault stale until the next interval tick.
        if (!this.syncing) this.scheduleSync('realtime connected', 1000)
      }
      socket.onmessage = (event) => {
        this.handleRealtimeMessage(event.data)
      }
      socket.onerror = () => {
        // The close handler schedules the reconnect; logging here would duplicate it.
      }
      socket.onclose = () => {
        this.socket = null
        if (!this.running || this.stopped) return
        if (!this.syncing) this.setStatus('offline')
        this.scheduleReconnect()
      }
    } catch (error) {
      this.options.log.warn(`cannot connect the realtime feed: ${describeError(error)}`)
      this.scheduleReconnect()
    }
  }

  private disconnectRealtime(): void {
    if (this.reconnectTimer !== null) window.clearTimeout(this.reconnectTimer)
    this.reconnectTimer = null
    if (this.socket) {
      this.socket.onclose = null
      this.socket.onmessage = null
      this.socket.onerror = null
      try {
        this.socket.close()
      } catch {
        /* already gone */
      }
      this.socket = null
    }
  }

  private scheduleReconnect(): void {
    if (!this.running || this.stopped || !this.options.getSettings().realtime) return
    const settings = this.options.getSettings()
    if (!settings.token) return
    if (this.reconnectTimer !== null) window.clearTimeout(this.reconnectTimer)
    const delay = Math.min(30_000, 1000 * 2 ** Math.min(this.reconnectAttempt, 5))
    this.reconnectAttempt += 1
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = null
      this.connectRealtime()
    }, delay)
  }

  private handleRealtimeMessage(raw: unknown): void {
    if (typeof raw !== 'string') return
    let message: { type?: string; change?: { kind?: string; path?: string; old_path?: string; etag?: string }; reason?: string }
    try {
      message = JSON.parse(raw)
    } catch {
      return
    }
    if (message.type === 'ready') {
      const ready = message as { root?: string }
      if (ready.root) this.root = ready.root
      this.options.log.info('realtime feed ready')
      return
    }
    if (message.type === 'resync') {
      this.options.log.warn(`server asked for a resync (${message.reason || 'unknown'})`)
      this.scheduleSync('server resync', 300)
      return
    }
    if (message.type !== 'change' || !message.change) return
    // A round is already in flight and may have fetched its manifest before this change landed.
    // Queue one follow-up instead of dropping the notification.
    if (this.syncing) {
      this.pending = true
      return
    }
    const change = message.change
    const path = change.path ? this.relativeToRoot(change.path) : null
    const oldPath = change.old_path ? this.relativeToRoot(change.old_path) : null
    if (path === null && oldPath === null) {
      // The server only forwards changes that touch this vault, so an unknown root (the `ready`
      // frame has not arrived yet) is still a reason to reconcile.
      if (!this.root) this.scheduleSync('remote change (unknown root)', 700)
      return
    }
    // The YSync session for an open note already applies these updates character by character.
    // Triggering a file-level round here would be pure overhead (and could race the CRDT).
    if (path && this.options.isRealtimePath?.(path)) return
    if (oldPath && this.options.isRealtimePath?.(oldPath)) return
    if (change.kind !== 'deleted' && change.kind !== 'moved' && path) {
      const baseline = this.options.getState().baseline[path]
      if (baseline?.etag && change.etag && baseline.etag === change.etag) return
    }
    // A write this client just performed is suppressed so its own echo cannot race the upload's
    // baseline update. Do not *drop* the notification though: another client may have edited the
    // same path in that window. Reconcile once the write has settled.
    const settling =
      (path !== null && this.isSuppressed(path)) || (oldPath !== null && this.isSuppressed(oldPath))
    this.scheduleSync(
      `remote change: ${path || oldPath || 'unknown'}`,
      settling ? 2600 : 700,
    )
  }

  private relativeToRoot(absolute: string): string | null {
    if (!this.root) return null
    if (absolute === this.root) return ''
    const prefix = `${this.root}/`
    if (!absolute.startsWith(prefix)) return null
    try {
      return normalizeVaultPath(absolute.slice(prefix.length))
    } catch {
      return null
    }
  }
}

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

function collectionBaseline(path: string): BaselineEntry {
  return {
    path,
    kind: 'collection',
    size: 0,
    mtime: 0,
    version: 0,
    content_type: '',
  }
}

function settingsHint(settings: MeiSyncSettings): string {
  if (!settings.serverUrl.trim()) return 'server URL not configured'
  return 'not signed in — set a username/password or token'
}

function emptySummary(reason: string, message: string): SyncSummary {
  return {
    reason,
    startedAt: Date.now(),
    durationMs: 0,
    uploads: 0,
    downloads: 0,
    deletesLocal: 0,
    deletesRemote: 0,
    mkdirs: 0,
    conflicts: 0,
    skipped: 0,
    errors: 0,
    ok: false,
    message,
  }
}

function describeAction(action: PlanAction): string {
  switch (action.type) {
    case 'download':
      return `download ${action.path}`
    case 'upload':
      return `upload ${action.path}${action.force ? ' (force)' : ''}`
    case 'delete-local':
      return `delete local ${action.path}`
    case 'delete-remote':
      return `delete remote ${action.path}`
    case 'mkdir-local':
      return `mkdir local ${action.path}`
    case 'mkdir-remote':
      return `mkdir remote ${action.path}`
    case 'conflict':
      return `conflict ${action.path} (${action.conflict})`
    case 'forget-baseline':
      return `forget baseline ${action.path}`
  }
}

function describeError(error: unknown): string {
  if (error instanceof SyncHttpError) {
    return error.status ? `HTTP ${error.status}: ${error.message}` : error.message
  }
  if (error instanceof Error) return error.message
  return String(error)
}

function isFatal(error: unknown): boolean {
  if (error instanceof SyncHttpError) {
    return error.status === 401 || error.status === 403 || error.status === 0
  }
  return false
}

async function sameBytes(a: ArrayBuffer, b: ArrayBuffer): Promise<boolean> {
  if (a.byteLength !== b.byteLength) return false
  const [left, right] = await Promise.all([sha256(a), sha256(b)])
  if (left && right) return left === right
  // Fall back to a byte comparison when SubtleCrypto is unavailable.
  const viewA = new Uint8Array(a)
  const viewB = new Uint8Array(b)
  for (let index = 0; index < viewA.length; index += 1) {
    if (viewA[index] !== viewB[index]) return false
  }
  return true
}

async function sha256(data: ArrayBuffer): Promise<string | null> {
  const subtle = globalThis.crypto?.subtle
  if (!subtle) return null
  try {
    const digest = await subtle.digest('SHA-256', data)
    return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, '0')).join('')
  } catch {
    return null
  }
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  const units = ['KB', 'MB', 'GB']
  let value = bytes
  let unit = -1
  do {
    value /= 1024
    unit += 1
  } while (value >= 1024 && unit < units.length - 1)
  return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unit]}`
}

export type { PlanResult }
