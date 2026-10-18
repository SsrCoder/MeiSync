import type { Editor } from '@tiptap/react'
import { useWorkspace } from '../Workspace'

interface ToolButton {
  label: string
  title: string
  active?: (editor: Editor) => boolean
  run: (editor: Editor) => void
}

const groups: ToolButton[][] = [
  [
    { label: 'H1', title: 'Heading 1', active: (e) => e.isActive('heading', { level: 1 }), run: (e) => e.chain().focus().toggleHeading({ level: 1 }).run() },
    { label: 'H2', title: 'Heading 2', active: (e) => e.isActive('heading', { level: 2 }), run: (e) => e.chain().focus().toggleHeading({ level: 2 }).run() },
    { label: 'H3', title: 'Heading 3', active: (e) => e.isActive('heading', { level: 3 }), run: (e) => e.chain().focus().toggleHeading({ level: 3 }).run() },
  ],
  [
    { label: 'B', title: 'Bold', active: (e) => e.isActive('bold'), run: (e) => e.chain().focus().toggleBold().run() },
    { label: 'I', title: 'Italic', active: (e) => e.isActive('italic'), run: (e) => e.chain().focus().toggleItalic().run() },
    { label: 'S', title: 'Strikethrough', active: (e) => e.isActive('strike'), run: (e) => e.chain().focus().toggleStrike().run() },
    { label: '<>', title: 'Inline code', active: (e) => e.isActive('code'), run: (e) => e.chain().focus().toggleCode().run() },
  ],
  [
    { label: '¶', title: 'Paragraph', active: (e) => e.isActive('paragraph'), run: (e) => e.chain().focus().setParagraph().run() },
    { label: '❝', title: 'Blockquote', active: (e) => e.isActive('blockquote'), run: (e) => e.chain().focus().toggleBlockquote().run() },
    { label: '{ }', title: 'Code block', active: (e) => e.isActive('codeBlock'), run: (e) => e.chain().focus().toggleCodeBlock().run() },
    { label: '—', title: 'Horizontal rule', run: (e) => e.chain().focus().setHorizontalRule().run() },
  ],
  [
    { label: '•', title: 'Bullet list', active: (e) => e.isActive('bulletList'), run: (e) => e.chain().focus().toggleBulletList().run() },
    { label: '1.', title: 'Ordered list', active: (e) => e.isActive('orderedList'), run: (e) => e.chain().focus().toggleOrderedList().run() },
  ],
]

export function Toolbar() {
  const { editor } = useWorkspace()
  return (
    <div className="toolbar">
      {groups.map((group, groupIndex) => (
        <span key={groupIndex} style={{ display: 'contents' }}>
          {groupIndex > 0 ? <span className="sep" /> : null}
          {group.map((button) => (
            <button
              key={button.label}
              title={button.title}
              disabled={!editor}
              className={editor && button.active?.(editor) ? 'active' : ''}
              onClick={() => editor && button.run(editor)}
            >
              {button.label}
            </button>
          ))}
        </span>
      ))}
      <span className="spacer" />
      <span className="muted mono" style={{ paddingLeft: 8 }}>
        {editor ? `${editor.state.doc.content.size} positions` : 'editor starting…'}
      </span>
    </div>
  )
}