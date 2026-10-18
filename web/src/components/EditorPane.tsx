import { EditorContent } from '@tiptap/react'
import { useWorkspace } from '../Workspace'

export function EditorPane() {
  const { editor, session, path, openError } = useWorkspace()
  return (
    <div className="editor-scroll">
      <div className="mono muted" style={{ marginBottom: 10 }}>
        {path}
        {editor?.isEditable === false ? ' · read only' : ''}
      </div>
      {openError ? <div className="banner error" style={{ marginBottom: 12 }}>{openError}</div> : null}
      <EditorContent editor={editor} />
      {!editor ? <div className="muted">Loading editor…</div> : null}
      {session.provider.status === 'denied' ? (
        <div className="banner error" style={{ marginTop: 12 }}>
          The server denied this document. It may have been deleted or moved.
        </div>
      ) : null}
    </div>
  )
}