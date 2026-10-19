import type { DiffPatch } from './text-diff'

/**
 * Structural subset of Obsidian's `Editor` / the CodeMirror 6 view behind it.
 *
 * Declared here instead of importing `obsidian` so this module stays a pure unit that the
 * test suite can exercise with plain objects.
 */
export interface EditorPositionLike {
  line: number
  ch: number
}

export interface EditorChangeLike {
  from: EditorPositionLike
  to?: EditorPositionLike
  text: string
}

export interface EditorTransactionLike {
  changes?: EditorChangeLike[]
}

export interface EditorScrollInfoLike {
  left: number
  top: number
}

export interface CodeMirrorViewLike {
  dispatch(spec: {
    changes: { from: number; to: number; insert: string }
    scrollIntoView?: boolean
    userEvent?: string
  }): void
}

export interface PatchableEditor {
  transaction(transaction: EditorTransactionLike, origin?: string): void
  offsetToPos(offset: number): EditorPositionLike
  getScrollInfo(): EditorScrollInfoLike
  scrollTo(x?: number | null, y?: number | null): void
  /** Undocumented but stable since CM6 (Obsidian 0.13): the underlying editor view. */
  cm?: CodeMirrorViewLike
}

/**
 * Apply a remote patch to an open editor **without moving that client's viewport**.
 *
 * `Editor.transaction()` cannot be used for remote edits: Obsidian dispatches it on its own
 * CodeMirror 6 view with a hard-coded `scrollIntoView: true`, so every incoming YSync update
 * scrolls the receiving pane back to its cursor — yanking the view away from wherever the user
 * is currently reading (and doing it once per pane that shows the note).
 *
 * Dispatching the same change on the underlying view with `scrollIntoView: false` leaves the
 * viewport alone. The cursor/selection does not have to be written by hand: CodeMirror maps the
 * existing selection through the change on its own, exactly like Obsidian's transaction does
 * with its explicit `selections`. CodeMirror positions are UTF-16 offsets into the document, so
 * the offsets from {@link DiffPatch} can be used unchanged.
 */
export function applyPatchKeepingViewport(
  editor: PatchableEditor,
  patch: DiffPatch,
  origin: string,
): void {
  const cm = editor.cm
  if (cm && typeof cm.dispatch === 'function') {
    try {
      cm.dispatch({
        changes: {
          from: patch.index,
          to: patch.index + patch.deleteCount,
          insert: patch.insert,
        },
        scrollIntoView: false,
        userEvent: origin,
      })
      return
    } catch {
      // The view rejected the change for some reason; fall back to the public API below, which
      // at least keeps the document in sync even if it restores the old scroll behaviour.
    }
  }
  applyWithTransaction(editor, patch, origin)
}

function applyWithTransaction(
  editor: PatchableEditor,
  patch: DiffPatch,
  origin: string,
): void {
  const from = editor.offsetToPos(patch.index)
  const to = patch.deleteCount > 0 ? editor.offsetToPos(patch.index + patch.deleteCount) : from
  const scroll = readScroll(editor)
  try {
    editor.transaction({ changes: [{ from, to, text: patch.insert }] }, origin)
  } finally {
    restoreScroll(editor, scroll)
  }
}

function readScroll(editor: PatchableEditor): EditorScrollInfoLike | null {
  try {
    return editor.getScrollInfo()
  } catch {
    return null
  }
}

function restoreScroll(editor: PatchableEditor, scroll: EditorScrollInfoLike | null): void {
  if (!scroll) return
  writeScroll(editor, scroll)
  // The forced scroll target may only be applied in CodeMirror's next measure pass.
  if (typeof requestAnimationFrame === 'function') {
    requestAnimationFrame(() => writeScroll(editor, scroll))
  }
}

function writeScroll(editor: PatchableEditor, scroll: EditorScrollInfoLike): void {
  try {
    editor.scrollTo(scroll.left, scroll.top)
  } catch {
    /* the editor may have been torn down in the meantime */
  }
}
