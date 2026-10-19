import { Notice, Platform, Plugin, setIcon, setTooltip } from 'obsidian'
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
  /** Status bar item owned by this plugin; only used when core Sync has no icon. */
  private ownStatusBarEl: HTMLElement | null = null
  /** Element the plugin-owned indicator lives in (its own item, or core Sync's item). */
  private statusHostEl: HTMLElement | null = null
  /** Clickable element created by this plugin (carries the tooltip). */
  private statusSegmentEl: HTMLElement | null = null
  /** Icon element currently driven by the plugin (our span or core Sync's icon). */
  private statusIconEl: HTMLElement | null = null
  /** Core Sync's icon element while it is reused instead of adding a duplicate icon. */
  private coreIconEl: HTMLElement | null = null
  private coreIconObserver: MutationObserver | null = null
  private statusObserver: MutationObserver | null = null
  private mobileDrawer: MobileDrawerHost | null = null
  private mobileHeader: HTMLElement | null = null
  private mobileButtonEl: HTMLElement | null = null
  private mobileObserver: MutationObserver | null = null
  private lastStatus: SyncStatus = 'idle'
  private lastDetail: string | undefined

  /** Open the log instead of letting core Sync open its menu for clicks on its status bar item. */
  private readonly statusClick = (event: MouseEvent): void => {
    event.stopPropagation()
    this.showLog()
  }

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

    // Reuse Obsidian's own sync indicator: the core Sync plugin renders a status bar icon on
    // desktop and a right-sidebar header button on mobile. Follow the same affordance, and only
    // create a plugin-owned status bar item when core Sync is off.
    if (Platform.isMobile) {
      // The right sidebar (and its header) only exists once the layout is ready.
      this.app.workspace.onLayoutReady(() => this.mountMobileStatus())
    } else {
      this.ownStatusBarEl = this.addStatusBarItem()
      this.mountStatusBar()
      this.observeStatusBar()
    }

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
    this.statusObserver?.disconnect()
    this.statusObserver = null
    this.mobileObserver?.disconnect()
    this.mobileObserver = null
    // Our own item is removed by the plugin lifecycle; a segment living inside core Sync's item
    // is ours to clean up, and core Sync's icon element stays theirs.
    this.releaseIndicator()
    this.mobileButtonEl = null
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
    this.lastStatus = status
    this.lastDetail = detail
    this.renderStatus()
  }

  /**
   * Render the state onto whichever icon element is currently in use.
   *
   * Writes are skipped when the element already looks right: the element may be Obsidian's core
   * Sync icon, which core keeps rewriting, and a non-idempotent render would loop with the
   * re-assertion observer below.
   */
  private renderStatus(): void {
    const icon = this.statusIconEl
    if (!icon) return
    const appearance = statusAppearance(this.lastStatus, this.settings)
    setIcon(icon, appearance.icon)
    for (const modifier of STATUS_MODIFIERS) {
      const wanted = appearance.modifiers.includes(modifier)
      if (icon.hasClass(modifier) !== wanted) icon.toggleClass(modifier, wanted)
    }
    const target = this.statusSegmentEl ?? icon
    const detail = this.lastDetail
    const label = detail ? `${appearance.label} — ${detail}` : appearance.label
    if (target.getAttribute('aria-label') !== label) setTooltip(target, label, { placement: 'top' })
  }

  private statusBarContainer(): HTMLElement | null {
    return (
      this.ownStatusBarEl?.parentElement ?? document.querySelector<HTMLElement>('.status-bar')
    )
  }

  /** The status bar item of Obsidian's core Sync plugin, when it is enabled. */
  private coreSyncStatusItem(): HTMLElement | null {
    const container = this.statusBarContainer()
    if (!container) return null
    return container.querySelector<HTMLElement>('.status-bar-item.plugin-sync')
  }

  /** Core Sync's own icon, ignoring the one this plugin created. */
  private findCoreSyncIcon(root: ParentNode | null): HTMLElement | null {
    if (!root) return null
    for (const icon of Array.from(root.querySelectorAll<HTMLElement>('.sync-status-icon'))) {
      if (!icon.closest('.meisync-status')) return icon
    }
    return null
  }

  /**
   * Drive core Sync's icon element itself, so the bar keeps exactly one sync icon. Core Sync is
   * free to update it again; the observer re-asserts our state right after such a write.
   */
  private adoptIcon(icon: HTMLElement): void {
    if (this.coreIconEl === icon) {
      this.renderStatus()
      return
    }
    this.releaseIndicator()
    this.coreIconEl = icon
    this.statusIconEl = icon
    icon.addEventListener('click', this.statusClick)
    this.coreIconObserver = new MutationObserver(() => this.renderStatus())
    this.coreIconObserver.observe(icon, {
      childList: true,
      attributes: true,
      attributeFilter: ['class'],
    })
    this.renderStatus()
  }

  private releaseIndicator(): void {
    this.coreIconObserver?.disconnect()
    this.coreIconObserver = null
    this.coreIconEl?.removeEventListener('click', this.statusClick)
    this.coreIconEl = null
    this.statusSegmentEl?.detach()
    this.statusSegmentEl = null
    this.statusIconEl = null
    this.statusHostEl = null
  }

  /**
   * Desktop indicator: reuse core Sync's icon when it exists (no duplicate icon), and fall back to
   * a status bar item of our own when core Sync is off. Core Sync creates/destroys its item when
   * the user toggles it, which {@link observeStatusBar} follows.
   */
  private mountStatusBar(): void {
    if (!this.ownStatusBarEl) return
    const coreItem = this.coreSyncStatusItem()
    if (coreItem) {
      // Our own item is redundant while core Sync provides one.
      if (this.ownStatusBarEl.isConnected) this.ownStatusBarEl.detach()
    } else if (!this.ownStatusBarEl.isConnected) {
      // Core Sync removed its item again (or none was ever there).
      document.querySelector<HTMLElement>('.status-bar')?.appendChild(this.ownStatusBarEl)
    }

    const coreIcon = this.findCoreSyncIcon(coreItem)
    if (coreIcon) {
      this.adoptIcon(coreIcon)
      return
    }

    const host = coreItem ?? this.ownStatusBarEl
    if (this.statusHostEl === host && this.statusSegmentEl?.parentElement === host) {
      this.renderStatus()
      return
    }
    this.releaseIndicator()
    const segment = host.createDiv('status-bar-item-segment meisync-status')
    segment.addEventListener('click', this.statusClick)
    segment.addEventListener('contextmenu', (event) => event.stopPropagation())
    this.statusSegmentEl = segment
    this.statusIconEl = segment.createSpan('status-bar-item-icon sync-status-icon')
    this.statusHostEl = host
    this.renderStatus()
  }

  /**
   * Mobile counterpart: core Sync renders its indicator as a right-sidebar header button there,
   * so reuse that button when it exists and only create our own when it does not.
   */
  private mountMobileStatus(): void {
    const drawer = this.app.workspace.rightSplit as unknown as MobileDrawerHost
    if (typeof drawer.addHeaderButton !== 'function') {
      this.log.warn('the mobile sidebar exposes no header buttons; the sync indicator is disabled')
      return
    }
    this.mobileDrawer = drawer
    this.mobileHeader = drawer.containerEl?.querySelector<HTMLElement>('.workspace-drawer-header') ?? null
    this.mountMobileIndicator()
    const header = this.mobileHeader
    if (header && !this.mobileObserver) {
      // Core Sync adds its header button on layout ready and removes it when it is disabled; watch
      // the drawer header (not the whole drawer) so the file tree inside it cannot spam us.
      this.mobileObserver = new MutationObserver(() => this.mountMobileIndicator())
      this.mobileObserver.observe(header, { childList: true, subtree: true })
    }
  }

  private mountMobileIndicator(): void {
    const drawer = this.mobileDrawer
    if (!drawer) return
    const coreIcon = this.findCoreSyncIcon(this.mobileHeader ?? drawer.containerEl ?? null)
    if (coreIcon) {
      if (this.mobileButtonEl) {
        this.mobileButtonEl.detach()
        this.mobileButtonEl = null
      }
      this.adoptIcon(coreIcon)
      return
    }
    if (this.coreIconEl) this.releaseIndicator()
    if (this.mobileButtonEl && !this.mobileButtonEl.isConnected) this.mobileButtonEl = null
    if (!this.mobileButtonEl) {
      if (typeof drawer.addHeaderButton !== 'function') return
      const button = drawer.addHeaderButton('sync-small', () => this.showLog())
      button.addClass('meisync-status', 'sync-status-icon')
      this.mobileButtonEl = button
    }
    if (this.statusIconEl !== this.mobileButtonEl) {
      this.statusIconEl = this.mobileButtonEl
      this.statusSegmentEl = this.mobileButtonEl
      this.renderStatus()
    }
  }

  private observeStatusBar(): void {
    const container = this.statusBarContainer()
    if (!container || this.statusObserver) return
    this.statusObserver = new MutationObserver(() => this.mountStatusBar())
    this.statusObserver.observe(container, { childList: true, subtree: true })
  }
}

/** Class set core Sync's `_updateStatusBar` toggles on its icon. */
const STATUS_MODIFIERS = ['mod-success', 'mod-working', 'mod-error', 'mod-spin'] as const

/** Undocumented mobile drawer surface used by core Sync for its header button. */
interface MobileDrawerHost {
  containerEl?: HTMLElement
  addHeaderButton?: (icon: string, callback: (event: MouseEvent) => unknown) => HTMLElement
}

/** Icon + colour for a status, matching `SyncPlugin._updateStatusBar` in Obsidian core. */
function statusAppearance(
  status: SyncStatus,
  settings: MeiSyncSettings,
): { icon: string; modifiers: string[]; label: string } {
  switch (status) {
    case 'syncing':
      return { icon: 'sync-small', modifiers: ['mod-working', 'mod-spin'], label: 'MeiSync: syncing…' }
    case 'error':
      return { icon: 'sync-small', modifiers: ['mod-error'], label: 'MeiSync: error' }
    case 'offline':
      return { icon: 'refresh-cw-off', modifiers: ['mod-error'], label: 'MeiSync: offline' }
    case 'disabled':
      if (!settings.serverUrl.trim()) {
        return { icon: 'sync-small', modifiers: ['mod-working'], label: 'MeiSync: setup required' }
      }
      if (!settings.token && !(settings.username.trim() && settings.password)) {
        return { icon: 'sync-small', modifiers: ['mod-working'], label: 'MeiSync: sign in required' }
      }
      return { icon: 'paused', modifiers: ['mod-working'], label: 'MeiSync: paused' }
    default:
      return { icon: 'check-small', modifiers: ['mod-success'], label: 'MeiSync: idle' }
  }
}

function clampNumber(value: number, min: number, max: number, fallback: number): number {
  const parsed = Number(value)
  if (!Number.isFinite(parsed)) return fallback
  return Math.max(min, Math.min(max, parsed))
}
