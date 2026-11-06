/**
 * Admin console vendor bundle entry (loaded lazily by `app.js` via dynamic import).
 *
 * The admin console is a dependency-free SPA embedded into the Rust binary, so this bundle is
 * built ahead of time and committed as `vendor/bundle.js` — see `web/scripts/build-admin-vendor.mjs`.
 * Everything the realtime file editor needs is re-exported here:
 *
 *   * the Yjs / lib0 / y-protocols pieces for the YSync wire protocol (same framing as the
 *     test client in `web/src/meisync/provider.ts`)
 *   * CodeMirror 6 plus `y-codemirror.next` for the collaborative editor binding
 *   * one language package per supported file type
 */
export * as Y from 'yjs'
export * as encoding from 'lib0/encoding'
export * as decoding from 'lib0/decoding'
export * as syncProtocol from 'y-protocols/sync'
export { Awareness, applyAwarenessUpdate, encodeAwarenessUpdate, removeAwarenessStates } from 'y-protocols/awareness'

export { yCollab, yUndoManagerKeymap } from 'y-codemirror.next'

export {
  EditorView,
  keymap,
  lineNumbers,
  highlightActiveLine,
  highlightActiveLineGutter,
  drawSelection,
  dropCursor,
  rectangularSelection,
  crosshairCursor,
  highlightSpecialChars,
} from '@codemirror/view'
export { EditorState, Compartment } from '@codemirror/state'
export { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands'
export { bracketMatching, indentOnInput, syntaxHighlighting, defaultHighlightStyle, StreamLanguage } from '@codemirror/language'
export { searchKeymap, highlightSelectionMatches, search } from '@codemirror/search'
export { closeBrackets, closeBracketsKeymap } from '@codemirror/autocomplete'
export { oneDark } from '@codemirror/theme-one-dark'

export { json } from '@codemirror/lang-json'
export { markdown } from '@codemirror/lang-markdown'
export { javascript } from '@codemirror/lang-javascript'
export { css } from '@codemirror/lang-css'
export { html } from '@codemirror/lang-html'
export { python } from '@codemirror/lang-python'
export { rust } from '@codemirror/lang-rust'
export { yaml } from '@codemirror/lang-yaml'
export { toml } from '@codemirror/legacy-modes/mode/toml'
export { go } from '@codemirror/legacy-modes/mode/go'
export { shell } from '@codemirror/legacy-modes/mode/shell'
