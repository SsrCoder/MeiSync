import { useEffect, useState } from 'react'
import { useWorkspace } from '../Workspace'
import type { FileNode } from '../meisync/api'

/**
 * Live view of the YSync connection plus the server's own view of the document (version, etag,
 * sync mode). `editor == Y.Text` is the invariant worth watching: the projection the editor
 * represents must be exactly what the shared text holds, otherwise the file on disk drifts.
 */
export function SessionPanel() {
  const { session, api, path, revision, bump, log } = useWorkspace()
  const { provider, binding, ytext } = session
  const [status, setStatus] = useState(provider.status)
  const [synced, setSynced] = useState(provider.synced)
  const [info, setInfo] = useState<FileNode | null>(null)
  const [textLength, setTextLength] = useState(ytext.length)
  const [editorText, setEditorText] = useState('')

  useEffect(() => {
    const offStatus = provider.on('status', setStatus)
    const offSynced = provider.on('synced', setSynced)
    return () => {
      offStatus()
      offSynced()
    }
  }, [provider])

  useEffect(() => {
    const timer = setInterval(() => {
      setTextLength(ytext.length)
      setEditorText(binding.editorText())
      api
        .getFile(path)
        .then(setInfo)
        .catch(() => setInfo(null))
    }, 1000)
    return () => clearInterval(timer)
  }, [ytext, binding, api, path, revision])

  const consistent = editorText === ytext.toString()

  return (
    <div className="section">
      <h2>YSync session</h2>
      <div className="row" style={{ flexWrap: 'wrap' }}>
        <span className={`badge ${status}`}>
          <span className="dot" />
          {status}
        </span>
        <span className="badge">{synced ? 'in sync' : 'syncing…'}</span>
        {info?.sync_mode === 'full' ? <span className="badge">full-state (base64)</span> : null}
      </div>
      {info?.sync_mode === 'full' ? (
        <div className="banner" style={{ marginTop: 8 }}>
          This file is stored base64 encoded inside the shared text. Editing it here would rewrite
          the encoding — reopen it only to inspect the raw text.
        </div>
      ) : null}
      <div className="stat-grid" style={{ marginTop: 8 }}>
        <span className="k">path</span>
        <span>{path}</span>
        <span className="k">clientID</span>
        <span>{session.doc.clientID}</span>
        <span className="k">ytext.length</span>
        <span>{textLength}</span>
        <span className="k">editor text</span>
        <span>{editorText.length}</span>
        <span className="k">projection</span>
        <span style={{ color: consistent ? 'var(--green)' : 'var(--yellow)' }}>
          {consistent ? 'editor == Y.Text' : 'editor != Y.Text'}
        </span>
        <span className="k">version</span>
        <span>{info?.version ?? '—'}</span>
        <span className="k">size</span>
        <span>{info?.size ?? '—'}</span>
        <span className="k">etag</span>
        <span>{info?.etag ? `${info.etag.slice(0, 16)}…` : '—'}</span>
        <span className="k">up in / out</span>
        <span>
          {provider.updatesReceived} / {provider.updatesSent}
        </span>
      </div>
      <div className="row" style={{ marginTop: 8, flexWrap: 'wrap' }}>
        <button
          onClick={() => {
            provider.disconnect()
            provider.connect()
            log('manual reconnect')
          }}
        >
          reconnect
        </button>
        <button
          onClick={() => {
            const count = (ytext.toString().match(/\n/g) ?? []).length + 1
            log(`local insert of 12 chars at index ${count}`)
            session.doc.transact(() => ytext.insert(Math.max(0, ytext.length), '\nlocal-tick\n'), 'test-button')
          }}
          title="Yjs transaction without going through the editor (binding sees it as remote)"
        >
          yjs insert
        </button>
        <button onClick={bump}>refresh</button>
      </div>
    </div>
  )
}