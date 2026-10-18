import { getSchema } from '@tiptap/core'
import StarterKit from '@tiptap/starter-kit'
import type { Schema } from 'prosemirror-model'

/**
 * The schema is intentionally the "markdown subset" of StarterKit: every enabled node/mark has a
 * representation in `meisync/markdown.ts`, and nothing else is allowed into the document — a
 * node we cannot serialise would be silently flattened by the projection.
 */
export const editorExtensions = [
  StarterKit.configure({
    heading: { levels: [1, 2, 3] },
    // Not part of the codec: a link or underline mark would be lost on materialisation.
    link: false,
    underline: false,
    // Adds a trailing empty paragraph, which would show up as a trailing newline in the file.
    trailingNode: false,
    dropcursor: { color: '#6ea8fe', width: 2 },
    undoRedo: { depth: 300 },
  }),
]

let cachedSchema: Schema | null = null

/** The ProseMirror schema (also used by the headless codec tests). */
export function editorSchema(): Schema {
  cachedSchema ??= getSchema(editorExtensions)
  return cachedSchema
}