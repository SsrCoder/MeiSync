/**
 * Binding between a Tiptap/ProseMirror editor and MeiSync's `Y.Text("content")`.
 *
 * Tiptap's stock `@tiptap/extension-collaboration` is built on y-prosemirror and stores the
 * document in a `Y.XmlFragment`. MeiSync's whole content model is a single `Y.Text("content")`
 * (§6): the server materialises files by reading that text, and its test suite asserts on
 * `txn.get_text("content")`. An `XmlFragment` would sync perfectly and then materialise as an
 * empty file, so this client projects the editor onto the shared text instead:
 *
 * ```text
 *   editor.doc  ──markdown codec──►  Y.Text("content")  ──ysync──►  MeiSync
 *       ▲                                    │
 *       └────────────── markdown codec ──────┘
 * ```
 *
 * Properties worth knowing:
 *
 * * **Convergence** — remote changes replace the editor content, local changes splice the smallest
 *   differing range into the shared text, so two clients editing different places of the same
 *   document merge without a conflict.
 * * **History** — remote updates are dispatched with `addToHistory: false`, so undo never replays
 *   somebody else's text.
 * * **Awareness** — the selection is published as a `Y.RelativePosition` on the shared text, which
 *   is why remote carets stay put while other people type.
 */
import { Plugin, PluginKey, TextSelection } from '@tiptap/pm/state'
import { Decoration, DecorationSet, type EditorView } from '@tiptap/pm/view'
import { Extension } from '@tiptap/core'
import type { Node as PMNode } from '@tiptap/pm/model'
import * as Y from 'yjs'
import type { Awareness } from 'y-protocols/awareness'
import { markdownToDoc, posToTextIndex, serializeDoc, textIndexToPos } from './markdown'

/** The shared text root of every MeiSync file. */
export const CONTENT_KEY = 'content'

/** Origin used for the transactions this binding creates (never sent back as a remote change). */
export const LOCAL_ORIGIN = Symbol('meisync-local')

export interface Peer {
  clientID: number
  name: string
  color: string
  /** Index into the shared text, or null when the peer has no selection published. */
  cursorIndex: number | null
}

export interface YTextBindingOptions {
  ydoc: Y.Doc
  /** Awareness channel used for cursors / presence. Omit to run without presence. */
  awareness?: Awareness | null
  /** Shared text root; MeiSync always uses `content`. */
  key?: string
  /** Local identity published in the awareness state. */
  user?: { name: string; color: string }
  /** Debug hook for the client's sync log. */
  onLog?: (message: string) => void
}

export interface YTextBinding {
  plugin: Plugin
  ytext: Y.Text
  /** Wire the binding to a mounted editor view (called once the editor exists). */
  attach(view: EditorView): void
  /** Peers currently visible in the room. */
  peers(): Peer[]
  setUser(user: { name: string; color: string }): void
  /** Text the editor currently represents (i.e. what the file would materialise as). */
  editorText(): string
  destroy(): void
}

interface BindingState {
  /** Bumped whenever the awareness channel changed, so decorations are recomputed. */
  awarenessVersion: number
}

export function createYTextBinding(options: YTextBindingOptions): YTextBinding {
  const { ydoc, awareness = null, onLog = () => {} } = options
  const ytext = ydoc.getText(options.key ?? CONTENT_KEY)
  const pluginKey = new PluginKey<BindingState>('meisync-ytext-binding')

  let view: EditorView | null = null
  let applyingRemote = false
  let remoteScheduled = false

  // ---------------------------------------------------------------- y.Text -> editor

  const applySharedText = (reason: string) => {
    if (!view) return
    const shared = ytext.toString()
    const state = view.state
    const local = serializeDoc(state.doc, false).text
    if (local === shared) return

    // Remember where the local caret sits in *text* coordinates so it survives the replacement.
    const before = serializeDoc(state.doc).positions
    const anchorIndex = posToTextIndex(before, state.selection.anchor)
    const headIndex = posToTextIndex(before, state.selection.head)

    const incoming = markdownToDoc(shared, state.schema)
    const tr = state.tr.replaceWith(0, state.doc.content.size, incoming.content)
    tr.setMeta(pluginKey, { source: 'remote', reason })
    tr.setMeta('addToHistory', false)

    const after = serializeDoc(tr.doc).positions
    const clamp = (pos: number) => Math.min(Math.max(textIndexToPos(after, pos), 0), tr.doc.content.size)
    const anchor = clamp(anchorIndex)
    const head = clamp(headIndex)
    try {
      tr.setSelection(TextSelection.create(tr.doc, anchor, head))
    } catch {
      /* the incoming document may be shorter; keep the mapped selection */
    }

    applyingRemote = true
    view.dispatch(tr)
    applyingRemote = false
    onLog(`applied remote change (${reason}): ${shared.length} chars`)
  }

  const scheduleRemoteApply = (reason: string) => {
    if (remoteScheduled) return
    remoteScheduled = true
    queueMicrotask(() => {
      remoteScheduled = false
      applySharedText(reason)
    })
  }

  const observer = (_event: Y.YTextEvent, transaction: Y.Transaction) => {
    if (transaction.origin === LOCAL_ORIGIN) return
    scheduleRemoteApply('ysync')
  }
  ytext.observe(observer)

  // ---------------------------------------------------------------- editor -> y.Text

  const pushEditorText = (doc: PMNode) => {
    if (applyingRemote) return
    const next = serializeDoc(doc, false).text
    const current = ytext.toString()
    if (current === next) return

    // Smallest differing range: common prefix + common suffix, then one delete + one insert.
    const patch = diffRange(current, next)
    if (!patch) return
    ydoc.transact(() => {
      if (patch.deleteCount > 0) ytext.delete(patch.index, patch.deleteCount)
      if (patch.insert.length > 0) ytext.insert(patch.index, patch.insert)
    }, LOCAL_ORIGIN)
  }

  // ---------------------------------------------------------------- awareness / carets

  const localUser = options.user ?? { name: 'anonymous', color: '#6ea8fe' }
  if (awareness) awareness.setLocalStateField('user', localUser)

  const publishCursor = (state: { doc: PMNode; selection: { anchor: number; head: number } }) => {
    if (!awareness) return
    const positions = serializeDoc(state.doc).positions
    const index = posToTextIndex(positions, state.selection.head)
    awareness.setLocalStateField(
      'cursor',
      Y.relativePositionToJSON(Y.createRelativePositionFromTypeIndex(ytext, index)),
    )
  }

  const buildDecorations = (state: EditorView['state']): DecorationSet => {
    if (!awareness || typeof document === 'undefined') return DecorationSet.empty
    const positions = serializeDoc(state.doc).positions
    const decorations: Decoration[] = []
    for (const [clientID, raw] of awareness.getStates()) {
      if (clientID === ydoc.clientID || !raw) continue
      const relative = decodeRelativePosition(raw.cursor)
      if (!relative) continue
      const absolute = Y.createAbsolutePositionFromRelativePosition(relative, ydoc)
      if (!absolute || absolute.index == null) continue
      const user = (raw.user as { name?: string; color?: string } | undefined) ?? undefined
      const name = user?.name ?? `peer ${clientID}`
      const color = user?.color ?? '#f28b82'
      const pos = clampPos(textIndexToPos(positions, absolute.index), state.doc.content.size)
      decorations.push(
        Decoration.widget(pos, () => {
          const caret = document.createElement('span')
          caret.className = 'meisync-caret'
          caret.style.backgroundColor = color
          const label = document.createElement('span')
          label.className = 'meisync-caret-label'
          label.style.backgroundColor = color
          label.textContent = name
          caret.appendChild(label)
          return caret
        }, { side: 1, key: `meisync-caret-${clientID}` }),
      )
    }
    return DecorationSet.create(state.doc, decorations)
  }

  const refreshDecorations = () => {
    if (!view) return
    view.dispatch(view.state.tr.setMeta(pluginKey, { source: 'awareness' }))
  }

  const onAwarenessChange = () => refreshDecorations()
  awareness?.on('change', onAwarenessChange)

  // ---------------------------------------------------------------- plugin

  const plugin = new Plugin<BindingState>({
    key: pluginKey,
    state: {
      init: (): BindingState => ({ awarenessVersion: 0 }),
      apply(tr, previous: BindingState): BindingState {
        return tr.getMeta(pluginKey) ? { awarenessVersion: previous.awarenessVersion + 1 } : previous
      },
    },
    props: {
      decorations(state) {
        return buildDecorations(state)
      },
    },
    view(pluginView) {
      view = pluginView
      publishCursor(pluginView.state)
      return {
        update(updatedView, previousState) {
          view = updatedView
          if (updatedView.state.doc !== previousState.doc) pushEditorText(updatedView.state.doc)
          if (!updatedView.state.selection.eq(previousState.selection)) publishCursor(updatedView.state)
        },
        destroy() {
          view = null
        },
      }
    },
  })

  return {
    plugin,
    ytext,
    attach(editorView) {
      view = editorView
      // The shared text may already hold content (server sent step2 before we mounted).
      applySharedText('initial')
      publishCursor(editorView.state)
    },
    peers(): Peer[] {
      if (!awareness) return []
      const peers: Peer[] = []
      for (const [clientID, raw] of awareness.getStates()) {
        const user = (raw?.user as { name?: string; color?: string } | undefined) ?? undefined
        const relative = decodeRelativePosition(raw?.cursor)
        const absolute = relative ? Y.createAbsolutePositionFromRelativePosition(relative, ydoc) : null
        peers.push({
          clientID,
          name: clientID === ydoc.clientID ? `${user?.name ?? 'me'} (you)` : (user?.name ?? `peer ${clientID}`),
          color: user?.color ?? '#9aa0a6',
          cursorIndex: absolute?.index ?? null,
        })
      }
      return peers.sort((a, b) => a.clientID - b.clientID)
    },
    setUser(user) {
      awareness?.setLocalStateField('user', user)
    },
    editorText() {
      return view ? serializeDoc(view.state.doc, false).text : ''
    },
    destroy() {
      ytext.unobserve(observer)
      awareness?.off('change', onAwarenessChange)
      view = null
    },
  }
}

/** Minimal diff: one replacement of the smallest differing window. */
export function diffRange(current: string, next: string): { index: number; deleteCount: number; insert: string } | null {
  if (current === next) return null
  let prefix = 0
  const maxPrefix = Math.min(current.length, next.length)
  while (prefix < maxPrefix && current[prefix] === next[prefix]) prefix++
  let suffix = 0
  const maxSuffix = Math.min(current.length, next.length) - prefix
  while (suffix < maxSuffix && current[current.length - 1 - suffix] === next[next.length - 1 - suffix]) suffix++
  return {
    index: prefix,
    deleteCount: current.length - prefix - suffix,
    insert: next.slice(prefix, next.length - suffix),
  }
}

/** Awareness states are JSON encoded, so the relative position arrives as plain JSON. */
function decodeRelativePosition(raw: unknown): Y.RelativePosition | null {
  if (!raw || typeof raw !== 'object') return null
  try {
    return Y.createRelativePositionFromJSON(raw)
  } catch {
    return null
  }
}

function clampPos(pos: number, max: number): number {
  return Math.min(Math.max(pos, 0), max)
}

/** Tiptap wrapper so the binding plugin can be passed in the `extensions` array. */
export function yTextBindingExtension(binding: YTextBinding): Extension {
  return Extension.create({
    name: 'yTextBinding',
    addProseMirrorPlugins: () => [binding.plugin],
  })
}