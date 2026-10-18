import { Notice, Plugin } from 'obsidian'
import { SyncEngine, type SyncStatus } from './engine'
import { HybridSyncManager } from './hybrid'
import { SyncLog } from './log'
import { SyncLogModal } from './log-modal'
import { MeiSyncSettingTab } from './settings'
import { createState } from './sync/state'
import {
  DEFAULT_SETTINGS,
  type MeiSyncSettings,
  type PersistedState,
  type PluginData,
} from './types'

export default class MeiSyncPlugin extends Plugin {
  override settings: MeiSyncSettings = { ...DEFAULT_SETTINGS }
  state: PersistedState = createState()
  log = new SyncLog()
  engine!: SyncEngine
  hybrid: HybridSyncManager | null = null
  private statusBarEl: HTMLElement | null = null

  override async onload(): Promise<void> {
    await this.loadPluginData()

    this.engine = new SyncEngine({
      app: this.app,
      log: this.log,
      pluginVersion: this.manifest.version,
      getSettings: () => this.settings,
      getState: () => this.state,
      onPersist: () => this.savePluginData(),
      onToken: async (token: string) => {
        this.settings.token = token
        await this.savePluginData()
        this.hybrid?.onSettingsChanged()
      },
      onStatus: (status, detail) => this.updateStatus(status, detail),
      isRealtimePath: (path: string) => this.hybrid?.manages(path) ?? false,
    })

    this.hybrid = new HybridSyncManager(this.app, this.engine, this.log, () => this.settings)

    this.statusBarEl = this.addStatusBarItem()
    this.statusBarEl.addClass('meisync-status')
    this.statusBarEl.setAttribute('aria-label', 'MeiSync')
    this.statusBarEl.onclick = () => this.showLog()
    this.updateStatus('idle')

    this.addRibbonIcon('refresh-cw', 'MeiSync: sync now', () => {
      void this.syncNow()
    })

    this.addCommand({
      id: 'meisync-sync-now',
      name: 'Sync now',
      callback: () => void this.syncNow(),
    })
    this.addCommand({
      id: 'meisync-show-log',
      name: 'Show sync log',
      callback: () => this.showLog(),
    })
    this.addCommand({
      id: 'meisync-reconnect',
      name: 'Reconnect realtime feed',
      callback: () => {
        this.engine.applySettings()
        new Notice('MeiSync: reconnecting')
      },
    })
    this.addCommand({
      id: 'meisync-reset-baseline',
      name: 'Reset sync baseline (dangerous)',
      callback: async () => {
        if (!window.confirm('Reset the MeiSync baseline for this vault? The next sync will reconcile from scratch and may create conflict copies.')) return
        await this.resetBaseline()
      },
    })

    this.addSettingTab(new MeiSyncSettingTab(this.app, this))

    // Hybrid realtime events are registered before the file-level handlers so an open note's
    // YSync session is closed before the engine performs a remote MOVE/DELETE.
    this.registerEvent(
      this.app.workspace.on('editor-change', (editor, info) => this.hybrid?.handleEditorChange(editor, info)),
    )
    this.registerEvent(this.app.workspace.on('active-leaf-change', () => this.hybrid?.scheduleActiveViews()))
    this.registerEvent(this.app.workspace.on('file-open', () => this.hybrid?.scheduleActiveViews()))
    this.registerEvent(this.app.workspace.on('layout-change', () => this.hybrid?.scheduleActiveViews()))
    this.registerEvent(
      this.app.vault.on('rename', (file, oldPath) => this.hybrid?.handleRename(file, oldPath)),
    )
    this.registerEvent(this.app.vault.on('delete', (file) => this.hybrid?.handleDelete(file.path)))

    this.registerEvent(this.app.vault.on('create', (file) => this.engine.handleLocalChange(file.path)))
    this.registerEvent(this.app.vault.on('modify', (file) => this.engine.handleLocalChange(file.path)))
    this.registerEvent(this.app.vault.on('delete', (file) => this.engine.handleLocalDelete(file.path)))
    this.registerEvent(
      this.app.vault.on('rename', (file, oldPath) => this.engine.handleLocalRename(file, oldPath)),
    )

    this.engine.start()
    this.hybrid.start()
    this.app.workspace.onLayoutReady(() => this.hybrid?.scheduleActiveViews(200))
    this.log.info(`plugin loaded (v${this.manifest.version})`)
  }

  override onunload(): void {
    this.hybrid?.stop()
    this.engine?.stop()
    // Best effort: persist the baseline even if the user disables the plugin mid-session.
    void this.savePluginData().catch(() => {})
    this.log.info('plugin unloaded')
  }

  onSettingsOpened(): void {
    // Typing a remote path should not create a folder for every intermediate value, and hybrid
    // sessions should not be restarted against a half-edited server URL.
    this.engine.setSuspended(true)
    this.hybrid?.setSuspended(true)
  }

  onSettingsClosed(): void {
    // Update the hybrid config/root key before resuming so it can decide whether a same-root
    // baseline refresh is safe.
    this.hybrid?.onSettingsChanged()
    this.engine.setSuspended(false)
    this.hybrid?.setSuspended(false)
    void this.onSettingsChanged()
  }

  async onSettingsChanged(): Promise<void> {
    // Keep numeric settings sane even when the user typed into an empty field.
    this.settings.syncIntervalSeconds = clampNumber(this.settings.syncIntervalSeconds, 10, 3600, 30)
    this.settings.maxFileSizeMb = clampNumber(this.settings.maxFileSizeMb, 1, 4096, 32)
    try {
      await this.savePluginData()
    } catch (error) {
      new Notice(`MeiSync: could not save settings — ${error instanceof Error ? error.message : String(error)}`, 8000)
    }
    this.engine.applySettings()
    this.hybrid?.onSettingsChanged()
  }

  async syncNow(): Promise<void> {
    await this.engine.syncNow('manual')
  }

  async testConnection(): Promise<{ user: string; root: string; server: string; devices: number }> {
    await this.savePluginData()
    this.engine.applySettings()
    return this.engine.testConnection()
  }

  showLog(): void {
    new SyncLogModal(this.app, this.log).open()
  }

  async resetBaseline(): Promise<void> {
    this.state.baseline = {}
    this.state.lastRoot = undefined
    this.state.lastManifestRevision = undefined
    this.state.lastSyncAt = undefined
    await this.savePluginData()
    this.engine.applySettings()
    new Notice('MeiSync: sync baseline reset')
    this.log.warn('sync baseline was reset')
  }

  async savePluginData(): Promise<void> {
    const data: PluginData = { settings: this.settings, state: this.state }
    await this.saveData(data)
  }

  private async loadPluginData(): Promise<void> {
    let raw: Partial<PluginData> | null = null
    try {
      raw = (await this.loadData()) as Partial<PluginData> | null
    } catch (error) {
      console.warn('[MeiSync] could not read plugin data; starting with defaults', error)
    }
    const settings = { ...DEFAULT_SETTINGS, ...(raw?.settings || {}) } as MeiSyncSettings
    settings.syncIntervalSeconds = clampNumber(settings.syncIntervalSeconds, 10, 3600, 30)
    settings.maxFileSizeMb = clampNumber(settings.maxFileSizeMb, 1, 4096, 32)
    settings.conflictStrategy = ['duplicate', 'manual', 'local-wins', 'remote-wins'].includes(
      settings.conflictStrategy,
    )
      ? settings.conflictStrategy
      : 'duplicate'
    settings.initialSync = ['merge', 'prefer-local', 'prefer-remote'].includes(settings.initialSync)
      ? settings.initialSync
      : 'merge'
    this.settings = settings
    this.state = createState(raw?.state)
  }

  private updateStatus(status: SyncStatus, detail?: string): void {
    if (!this.statusBarEl) return
    const text =
      status === 'syncing'
        ? 'MeiSync: syncing…'
        : status === 'error'
          ? 'MeiSync: error'
          : status === 'offline'
            ? 'MeiSync: offline'
            : status === 'disabled'
              ? !this.settings.serverUrl
                ? 'MeiSync: setup'
                : !this.settings.token && !(this.settings.username && this.settings.password)
                  ? 'MeiSync: sign in'
                  : 'MeiSync: paused'
              : 'MeiSync: idle'
    this.statusBarEl.setText(text)
    this.statusBarEl.setAttribute('title', detail ? `MeiSync — ${detail}` : text)
    this.statusBarEl.toggleClass('is-syncing', status === 'syncing')
    this.statusBarEl.toggleClass('is-error', status === 'error')
    this.statusBarEl.toggleClass('is-offline', status === 'offline')
  }
}

function clampNumber(value: number, min: number, max: number, fallback: number): number {
  const parsed = Number(value)
  if (!Number.isFinite(parsed)) return fallback
  return Math.max(min, Math.min(max, parsed))
}
