import { App, Editor, MarkdownFileInfo, MarkdownView, Notice, TAbstractFile, TFile } from 'obsidian'
import * as Y from 'yjs'
import { Awareness } from 'y-protocols/awareness'
import type { SyncEngine } from './engine'
import type { SyncLog } from './log'
import { isExcluded, normalizeBaseUrl, normalizeVaultPath } from './sync/paths'
import { diffRange, mapOffset } from './sync/text-diff'
import { MeiSyncProvider, ysyncUrl } from './sync/ysync-provider'
import type { MeiSyncSettings } from './types'

/** Y.Text root of every MeiSync file. */
const CONTENT_KEY = 'content'

/** Origin used for editor -> Y.Text transactions, so the provider is not echoed back. */
const LOCAL_ORIGIN = Symbol('meisync-obsidian-local')

const READY_TIMEOUT_MS = 12_000
const SYNCED_BLOCK_MS = 4_000

interface FlushResult {
  text: string | null
  saved: boolean
}

interface RealtimeSession {
  path: string
  absolutePath: string
  doc: Y.Doc
  ytext: Y.Text
  awareness: Awareness
  provider: MeiSyncProvider
  editors: Set<Editor>
  initialTexts: Map<Editor, string>
  ready: boolean
  applyingRemote: number
  applyPending: boolean
  timeout: number | null
}

/**
 * Hybrid realtime sync.
 *
 * Open Markdown notes use the normal MeiSync YSync WebSocket and the shared `Y.Text("content")`.
 * Attachments, `.obsidian` config and closed notes stay on the file-level engine. While a note is
 * open here, the file-level engine is told to ignore its path, so a whole-file PUT can never
 * overwrite a CRDT edit.
 */
export class HybridSyncManager {
  private sessions = new Map<string, RealtimeSession>()
  private starting = new Set<string>()
  private blockedUntil = new Map<string, number>()
  private protectedPaths = new Set<string>()
  private running = false
  private suspended = false
  private refreshTimer: number | null = null
  private configKey = ''
  private rootKey = ''

  constructor(
    private readonly app: App,
    private readonly engine: SyncEngine,
    private readonly log: SyncLog,
    private readonly getSettings: () => MeiSyncSettings,
  ) {}

  start(): void {
    if (this.running) return
    this.running = true
    this.suspended = false
    this.blockedUntil.clear()
    this.protectedPaths.clear()
    this.configKey = this.currentConfigKey()
    this.rootKey = this.currentRootKey()
    this.scheduleActiveViews(100)
  }

  stop(): void {
    this.running = false
    this.suspended = false
    if (this.refreshTimer !== null) window.clearTimeout(this.refreshTimer)
    this.refreshTimer = null
    this.stopAll('settings', true)
  }

  /** Paths the file-level engine must not touch. */
  managedPaths(): Set<string> {
    const paths = new Set<string>(this.sessions.keys())
    for (const path of this.starting) paths.add(path)
    for (const path of this.protectedPaths) paths.add(path)
    return paths
  }

  manages(path: string): boolean {
    return this.sessions.has(path) || this.starting.has(path) || this.protectedPaths.has(path)
  }

  setSuspended(suspended: boolean): void {
    this.suspended = suspended
    if (!suspended && this.running) this.scheduleActiveViews(150)
  }

  onSettingsChanged(): void {
    const key = this.currentConfigKey()
    const rootKey = this.currentRootKey()
    if (key === this.configKey) {
      this.scheduleActiveViews(150)
      return
    }
    const rootChanged = rootKey !== this.rootKey
    this.configKey = key
    this.rootKey = rootKey
    // A token-only change keeps the same remote root, so refreshing the affected baselines is
    // safe. A server/root change deliberately skips it; the next targeted sync resets through
    // the engine's normal root-change handling.
    this.stopAll('settings', !rootChanged)
    if (this.running && !this.suspended) this.scheduleActiveViews(150)
  }

  scheduleActiveViews(delay = 120): void {
    if (!this.running || this.suspended) return
    if (this.refreshTimer !== null) window.clearTimeout(this.refreshTimer)
    this.refreshTimer = window.setTimeout(() => {
      this.refreshTimer = null
      this.syncActiveViews()
    }, delay)
  }

  private currentConfigKey(): string {
    const settings = this.getSettings()
    return [this.currentRootKey(), settings.token, settings.hybridRealtime ? '1' : '0'].join('|')
  }

  private currentRootKey(): string {
    const settings = this.getSettings()
    return [settings.serverUrl, settings.remoteRoot, settings.vaultNameOverride].join('|')
  }

  private syncActiveViews(): void {
    if (!this.running || this.suspended) return
    const settings = this.getSettings()
    if (!settings.hybridRealtime) {
      this.stopAll('settings', true)
      return
    }
    if (!settings.serverUrl.trim() || (!settings.token && !(settings.username.trim() && settings.password))) {
      return
    }
    const now = Date.now()
    for (const [path, until] of this.blockedUntil) {
      if (until <= now) this.blockedUntil.delete(path)
    }
    const views = this.currentViews(settings)
    for (const [path, editors] of views) {
      if (this.blockedUntil.has(path) || this.protectedPaths.has(path)) continue
      const session = this.sessions.get(path)
      if (session) {
        session.editors = new Set(editors)
        if (session.ready) this.applyRemote(session)
        continue
      }
      if (!this.starting.has(path)) void this.activate(path, editors)
    }
    for (const path of [...this.sessions.keys()]) {
      if (!views.has(path)) this.stopSession(path, 'close')
    }
    for (const path of [...this.starting]) {
      if (!views.has(path)) this.starting.delete(path)
    }
  }

  private currentViews(settings: MeiSyncSettings): Map<string, Editor[]> {
    const views = new Map<string, Editor[]>()
    for (const leaf of this.app.workspace.getLeavesOfType('markdown')) {
      const view = leaf.view
      if (!(view instanceof MarkdownView)) continue
      const file = view.file
      const editor = view.editor
      if (
        !(file instanceof TFile) ||
        !['md', 'markdown'].includes(file.extension.toLowerCase()) ||
        !editor
      ) {
        continue
      }
      let path: string
      try {
        path = normalizeVaultPath(file.path)
      } catch {
        continue
      }
      if (!path || isExcluded(path, settings)) continue
      const existing = views.get(path)
      if (existing) existing.push(editor)
      else views.set(path, [editor])
    }
    return views
  }

  private async activate(path: string, editors: Editor[]): Promise<void> {
    if (!this.running || this.sessions.has(path) || this.starting.has(path) || this.protectedPaths.has(path)) return
    this.starting.add(path)
    const initialTexts = new Map<Editor, string>()
    for (const editor of editors) initialTexts.set(editor, editor.getValue())
    try {
      const settings = this.getSettings()
      const { absolutePath } = await this.engine.ensureFileForRealtime(path)
      if (
        !this.running ||
        this.suspended ||
        !this.getSettings().hybridRealtime ||
        !this.starting.has(path) ||
        !this.currentViews(this.getSettings()).has(path)
      ) {
        return
      }

      const doc = new Y.Doc()
      const ytext = doc.getText(CONTENT_KEY)
      const awareness = new Awareness(doc)
      const user = {
        name: settings.deviceName.trim() || `Obsidian · ${this.app.vault.getName()}`,
        color: colorFor(`${this.app.vault.getName()}/${settings.deviceName}`),
      }
      awareness.setLocalStateField('user', user)

      const provider = new MeiSyncProvider({
        url: ysyncUrl(normalizeBaseUrl(settings.serverUrl), absolutePath),
        doc,
        token: settings.token,
        awareness,
      })
      const session: RealtimeSession = {
        path,
        absolutePath,
        doc,
        ytext,
        awareness,
        provider,
        editors: new Set(editors),
        initialTexts,
        ready: false,
        applyingRemote: 0,
        applyPending: false,
        timeout: null,
      }
      this.sessions.set(path, session)

      ytext.observe((_event, transaction) => {
        if (transaction.origin === LOCAL_ORIGIN) return
        if (this.sessions.get(path) !== session) return
        this.applyRemote(session)
      })

      provider.on('synced', (synced) => {
        if (!synced) return
        try {
          this.markReady(session)
        } catch (error) {
          this.log.error(`could not bind ${path} to YSync: ${String(error)}`)
          this.stopSession(path, 'timeout')
        }
      })
      provider.on('status', (status) => {
        if (status === 'connected') this.log.info(`ysync connected: ${path}`)
      })
      provider.on('denied', (reason) => {
        this.log.warn(`ysync access denied for ${path}: ${reason}`)
        new Notice(`MeiSync: realtime access to ${path} was denied (${reason})`)
        this.block(path)
        this.stopSession(path, 'denied')
        this.engine.scheduleSync('ysync denied', 400)
      })
      provider.on('moved', (newPath) => {
        this.log.warn(`ysync document moved remotely: ${path} -> ${newPath}`)
        this.block(path)
        this.stopSession(path, 'moved')
        this.engine.scheduleSync('ysync moved', 400)
      })
      provider.on('error', (message) => {
        this.log.warn(`ysync ${path}: ${message}`)
      })

      session.timeout = window.setTimeout(() => {
        if (!session.ready && this.sessions.get(path) === session) {
          this.log.warn(`ysync did not finish the initial sync for ${path}; falling back to file sync`)
          this.stopSession(path, 'timeout')
          this.engine.scheduleSync('ysync timeout', 500)
        }
      }, READY_TIMEOUT_MS)
    } catch (error) {
      this.log.warn(
        `could not open a realtime session for ${path}: ${error instanceof Error ? error.message : String(error)}`,
      )
      if (!this.suspended) this.block(path, 15_000)
    } finally {
      this.starting.delete(path)
      if (!this.sessions.has(path) && this.running) {
        // The YSync session did not start; make sure the note's local changes are reconciled by
        // the file-level engine instead of waiting for the next interval.
        this.engine.scheduleSync('realtime fallback', 500)
      }
    }
  }

  private markReady(session: RealtimeSession): void {
    if (session.ready || this.sessions.get(session.path) !== session) return
    session.ready = true
    if (session.timeout !== null) {
      window.clearTimeout(session.timeout)
      session.timeout = null
    }
    this.log.info(`ysync ready: ${session.path}`)
    // Distinguish "the user typed while the WebSocket was connecting" from "the targeted sync
    // downloaded newer remote content and the editor is still showing the old file". In the
    // latter case the editor is unchanged from the pre-session snapshot and remote wins.
    const shared = session.ytext.toString()
    const changedEditor = [...session.editors].find((editor) => {
      if (!this.editorMatchesPath(editor, session.path)) return false
      const current = editor.getValue()
      // Obsidian may already have reloaded the file written by the targeted sync, in which case
      // the editor differs from the pre-session snapshot but already matches Y.Text. That is not
      // a local edit.
      if (current === shared) return false
      const initial = session.initialTexts.get(editor)
      if (initial === undefined) return true
      return current !== initial
    })
    if (changedEditor) {
      const initial = session.initialTexts.get(changedEditor)
      if (initial !== undefined && shared !== initial) {
        this.log.warn(
          `both local and remote text changed while opening ${session.path}; local edits take precedence`,
        )
      }
      this.pushLocal(session, changedEditor)
    }
    this.applyRemote(session)
  }

  handleEditorChange(editor: Editor, info: MarkdownFileInfo): void {
    if (!this.running || !info.file) return
    let path: string
    try {
      path = normalizeVaultPath(info.file.path)
    } catch {
      return
    }
    const session = this.sessions.get(path)
    if (!session || !session.ready) return
    if (session.applyingRemote > 0) return
    // Remote transactions may deliver their editor-change after the flag has been cleared. In
    // that case the editor already equals Y.Text and there is nothing to push.
    if (!session.editors.has(editor)) {
      if (!this.editorMatchesPath(editor, path)) return
      session.editors.add(editor)
    }
    try {
      if (editor.getValue() === session.ytext.toString()) return
      this.pushLocal(session, editor)
      this.applyRemote(session)
    } catch (error) {
      this.log.warn(
        `could not push local edit for ${path}: ${error instanceof Error ? error.message : String(error)}`,
      )
    }
  }

  handleRename(file: TAbstractFile, oldPath: string): void {
    let from: string
    try {
      from = normalizeVaultPath(oldPath)
    } catch {
      return
    }
    const prefix = `${from}/`
    let to = ''
    try {
      to = normalizeVaultPath(file.path)
    } catch {
      /* the file-level engine will handle an invalid path */
    }
    const movedNotes: Array<{ from: string; to: string }> = []
    for (const path of [...this.sessions.keys()]) {
      if (path === from || path.startsWith(prefix)) {
        const suffix = path === from ? '' : path.slice(from.length)
        if (to) movedNotes.push({ from: path, to: `${to}${suffix}` })
        this.stopSession(path, 'rename')
      }
    }
    for (const path of [...this.starting]) {
      if (path === from || path.startsWith(prefix)) this.starting.delete(path)
    }
    for (const path of [...this.protectedPaths]) {
      if (path === from || path.startsWith(prefix)) this.protectedPaths.delete(path)
    }
    if (to) this.block(to, 10_000)
    for (const note of movedNotes) this.block(note.to, 10_000)
    this.log.info(`realtime note renamed: ${oldPath} -> ${file.path}`)
    // Wait for the engine's remote MOVE before attaching a new YSync session to the new path,
    // otherwise the targeted pre-sync could race with the MOVE request.
    void this.resumeAfterRename(from, to, movedNotes)
  }

  private async resumeAfterRename(
    from: string,
    to: string,
    movedNotes: Array<{ from: string; to: string }>,
  ): Promise<void> {
    await sleep(150)
    for (let attempt = 0; attempt < 50 && this.running; attempt += 1) {
      if (!this.engine.isMoving(from)) break
      await sleep(100)
    }
    if (!this.running) return
    if (to) {
      // The engine remaps the *old* baseline etag through MOVE. Because a live YSync session has
      // advanced the remote ETag since that baseline, refresh the affected notes before attaching
      // sessions again; otherwise the pre-sync would see both sides changed and create conflicts.
      await sleep(150)
      if (movedNotes.length === 0) {
        await this.engine.refreshBaselineForPath(to)
      } else {
        for (const note of movedNotes) {
          await this.engine.refreshBaselineForPath(note.to)
        }
      }
      this.blockedUntil.delete(to)
      for (const note of movedNotes) this.blockedUntil.delete(note.to)
    }
    this.scheduleActiveViews(250)
  }

  handleDelete(path: string): void {
    let normalized: string
    try {
      normalized = normalizeVaultPath(path)
    } catch {
      return
    }
    const prefix = `${normalized}/`
    for (const sessionPath of [...this.sessions.keys()]) {
      if (sessionPath === normalized || sessionPath.startsWith(prefix)) {
        this.stopSession(sessionPath, 'delete')
      }
    }
    for (const sessionPath of [...this.starting]) {
      if (sessionPath === normalized || sessionPath.startsWith(prefix)) {
        this.starting.delete(sessionPath)
      }
    }
    for (const sessionPath of [...this.protectedPaths]) {
      if (sessionPath === normalized || sessionPath.startsWith(prefix)) {
        this.protectedPaths.delete(sessionPath)
      }
    }
    this.scheduleActiveViews(150)
  }

  /** An Editor object is reused when a leaf switches files; never write note A's session into
   * an editor that now displays note B. */
  private editorMatchesPath(editor: Editor, path: string): boolean {
    for (const leaf of this.app.workspace.getLeavesOfType('markdown')) {
      const view = leaf.view
      if (view instanceof MarkdownView && view.editor === editor) {
        try {
          return normalizeVaultPath(view.file?.path ?? '') === path
        } catch {
          return false
        }
      }
    }
    return false
  }

  private pushLocal(session: RealtimeSession, editor: Editor): void {
    if (session.applyingRemote > 0) return
    const current = editor.getValue()
    const shared = session.ytext.toString()
    if (current === shared) return
    const patch = diffRange(shared, current)
    if (!patch) return
    session.doc.transact(() => {
      if (patch.deleteCount > 0) session.ytext.delete(patch.index, patch.deleteCount)
      if (patch.insert) session.ytext.insert(patch.index, patch.insert)
    }, LOCAL_ORIGIN)
  }

  private applyRemote(session: RealtimeSession): void {
    if (!session.ready) return
    if (session.applyingRemote > 0) {
      session.applyPending = true
      queueMicrotask(() => {
        if (this.sessions.get(session.path) === session && session.applyPending) {
          session.applyPending = false
          this.applyRemote(session)
        }
      })
      return
    }
    const shared = session.ytext.toString()
    session.applyingRemote += 1
    try {
      for (const editor of session.editors) {
        try {
          if (!this.editorMatchesPath(editor, session.path)) continue
          const current = editor.getValue()
          if (current === shared) continue
          const patch = diffRange(current, shared)
          if (!patch) continue
          const from = editor.offsetToPos(patch.index)
          const to =
            patch.deleteCount > 0 ? editor.offsetToPos(patch.index + patch.deleteCount) : from
          const selections = editor.listSelections().map((selection) => ({
            from: editor.offsetToPos(mapOffset(editor.posToOffset(selection.anchor), patch)),
            to: editor.offsetToPos(mapOffset(editor.posToOffset(selection.head), patch)),
          }))
          try {
            editor.transaction(
              {
                changes: [{ from, to, text: patch.insert }],
                selections,
              },
              'meisync-ysync',
            )
          } catch (error) {
            // A stale selection should never block the text update.
            this.log.warn(
              `could not apply realtime selection update: ${error instanceof Error ? error.message : String(error)}`,
            )
            try {
              editor.transaction({ changes: [{ from, to, text: patch.insert }] }, 'meisync-ysync')
            } catch (retry) {
              this.log.error(
                `could not apply realtime update: ${retry instanceof Error ? retry.message : String(retry)}`,
              )
            }
          }
        } catch (error) {
          this.log.warn(
            `could not inspect editor for ${session.path}: ${error instanceof Error ? error.message : String(error)}`,
          )
        }
      }
    } finally {
      session.applyingRemote = Math.max(0, session.applyingRemote - 1)
    }
    if (session.applyPending) {
      session.applyPending = false
      queueMicrotask(() => {
        if (this.sessions.get(session.path) === session) this.applyRemote(session)
      })
    }
  }

  /**
   * Persist the editor's final text before measuring the file for `refreshBaselineForPath`.
   * Without this, a closing editor whose autosave has not fired yet can leave a stale mtime/size
   * in the baseline; a later file-level round would then upload stale disk content over the YDoc.
   */
  private async flushSession(session: RealtimeSession): Promise<FlushResult> {
    const editor = [...session.editors].find((candidate) =>
      this.editorMatchesPath(candidate, session.path),
    )
    if (!editor) return { text: null, saved: false }
    let text: string
    try {
      text = editor.getValue()
    } catch (error) {
      this.log.warn(`could not read ${session.path} before closing YSync: ${String(error)}`)
      return { text: null, saved: false }
    }
    const file = this.app.vault.getAbstractFileByPath(session.path)
    if (!(file instanceof TFile)) return { text, saved: false }
    try {
      const disk = await this.app.vault.read(file)
      if (disk === text) return { text, saved: true }
      await this.app.vault.adapter.write(session.path, text)
      return { text, saved: true }
    } catch (error) {
      this.log.warn(`could not save ${session.path} before closing YSync: ${String(error)}`)
      return { text, saved: false }
    }
  }

  private block(path: string, ms = SYNCED_BLOCK_MS): void {
    const until = Date.now() + ms
    this.blockedUntil.set(path, until)
    window.setTimeout(() => {
      if ((this.blockedUntil.get(path) ?? 0) <= Date.now()) {
        this.blockedUntil.delete(path)
        this.scheduleActiveViews(100)
      }
    }, ms + 50)
  }

  private stopSession(
    path: string,
    reason: 'close' | 'rename' | 'delete' | 'denied' | 'moved' | 'timeout' | 'settings',
    allowRefresh = false,
  ): void {
    const session = this.sessions.get(path)
    if (!session) return
    this.sessions.delete(path)
    let localText: string | null = null
    try {
      const editor = [...session.editors].find((candidate) =>
        this.editorMatchesPath(candidate, session.path),
      )
      if (editor) localText = editor.getValue()
    } catch {
      /* the editor may already be torn down */
    }
    // Capture this before provider.destroy(): destroy() marks the provider disconnected.
    const healthy = session.provider.synced && session.provider.status === 'connected'
    // Never refresh a baseline through the manifest API while Settings is open: the manifest
    // endpoint provisions the root, which must not happen for a half-typed remote path.
    const refresh =
      session.ready &&
      (reason === 'settings'
        ? allowRefresh
        : !this.suspended && (reason === 'close' || reason === 'timeout'))
    // Protect the path *before* flushing: writing the final editor text to disk fires a vault
    // `modify` event, and the file-level engine must not start a competing round for it.
    if (refresh) {
      this.protectedPaths.add(path)
      this.block(path, 10_000)
    }
    const flushed: Promise<FlushResult> =
      reason === 'rename' || reason === 'delete'
        ? Promise.resolve({ text: null, saved: false })
        : this.flushSession(session)
    if (session.timeout !== null) window.clearTimeout(session.timeout)
    session.provider.destroy()
    session.doc.destroy()
    session.awareness.destroy()
    if (refresh) {
      // Persist the editor text first (only if it differs from disk), then measure the file for
      // the baseline. Keep the path protected from file-level sync and reactivation until the
      // baseline is current; a transient layout change must not race the old baseline.
      void (async () => {
        try {
          const result = await flushed
          await sleep(150)
          const finalText = result.text ?? localText
          if (result.saved && finalText !== null) {
            await this.engine.finalizeRealtimePath(path, finalText, healthy)
          } else if (finalText !== null) {
            // The editor was already showing another file or could not be saved. Use the text we
            // did obtain, but never upload from a potentially stale editor: allowUpload=false
            // makes finalize refresh the baseline only when server and local text are equal and
            // otherwise leave the file-level conflict path in charge.
            this.log.warn(`could not persist ${path} before closing YSync; using conflict-safe finalize`)
            await this.engine.finalizeRealtimePath(path, finalText, false)
          } else {
            const file = this.app.vault.getAbstractFileByPath(path)
            if (file instanceof TFile) {
              const disk = await this.app.vault.read(file)
              await this.engine.finalizeRealtimePath(path, disk, false)
            } else {
              this.engine.scheduleSync('realtime finalize missing file', 300)
            }
          }
        } finally {
          this.protectedPaths.delete(path)
          this.blockedUntil.delete(path)
          this.scheduleActiveViews(80)
        }
      })().catch((error) => {
        this.log.warn(`realtime close cleanup failed for ${path}: ${String(error)}`)
      })
    } else {
      void flushed
    }
    if (reason === 'timeout' || reason === 'moved' || reason === 'denied') {
      this.block(path)
    }
    this.log.info(`realtime session closed (${reason}): ${path}`)
    if (this.running) this.scheduleActiveViews(150)
  }

  private stopAll(reason: 'settings' | 'close', allowRefresh = false): void {
    for (const path of [...this.sessions.keys()]) this.stopSession(path, reason, allowRefresh)
    this.starting.clear()
  }
}

// ---------------------------------------------------------------------------------------------
// text diff / selection helpers
// ---------------------------------------------------------------------------------------------

const COLORS = ['#6ea8fe', '#a78bfa', '#5dd6a0', '#f5c451', '#ff9e6d', '#ff7a9c']

function colorFor(seed: string): string {
  let hash = 0
  for (let index = 0; index < seed.length; index += 1) {
    hash = (hash * 31 + seed.charCodeAt(index)) >>> 0
  }
  return COLORS[hash % COLORS.length]
}
