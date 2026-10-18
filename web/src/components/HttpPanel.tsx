import { useCallback, useEffect, useState } from 'react'
import { useWorkspace } from '../Workspace'

/**
 * Drives the *other* protocol front ends against the same YDoc, which is the fastest way to see
 * that all three paths converge: type in the editor, `GET /dav` here; `PUT` a body here, watch
 * the editor re-render through YSync.
 */
export function HttpPanel() {
  const { api, path, bump, log, revision } = useWorkspace()
  const [body, setBody] = useState('')
  const [dav, setDav] = useState<string | null>(null)
  const [etag, setEtag] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [documentJson, setDocumentJson] = useState<string | null>(null)

  const loadDav = useCallback(async () => {
    try {
      const result = await api.davGet(path)
      setDav(result.text)
      setEtag(result.etag)
      setError(null)
    } catch (err) {
      setDav(null)
      setError(String(err))
    }
  }, [api, path])

  useEffect(() => {
    void loadDav()
    void api
      .document(path)
      .then((value) => setDocumentJson(JSON.stringify(value, null, 2)))
      .catch(() => setDocumentJson(null))
    setBody('')
  }, [loadDav, api, path, revision])

  // Keep the materialised view fresh: it is the same YDoc the editor writes to, so a poll shows
  // exactly what the server hands to WebDAV / S3 clients at this moment.
  useEffect(() => {
    const timer = setInterval(() => {
      void loadDav()
    }, 2000)
    return () => clearInterval(timer)
  }, [loadDav])

  const put = async () => {
    try {
      const response = await api.davPut(path, body)
      if (!response.ok) throw new Error(`PUT -> ${response.status} ${await response.text()}`)
      log(`WebDAV PUT ${path} (${body.length} bytes) — watch the editor update via ysync`)
      setError(null)
      bump()
    } catch (err) {
      setError(String(err))
    }
  }

  const appendRemote = async () => {
    const line = `\nappended over WebDAV at ${new Date().toISOString()}\n`
    try {
      const current = (await api.davGet(path)).text
      const response = await api.davPut(path, current + line)
      if (!response.ok) throw new Error(`PUT -> ${response.status} ${await response.text()}`)
      log('WebDAV PUT appended a line — the editor should follow without a reload')
      bump()
    } catch (err) {
      setError(String(err))
    }
  }

  return (
    <div className="section">
      <h2>
        HTTP front ends
        <span className="spacer" />
        <button onClick={() => void loadDav()}>GET /dav</button>
      </h2>
      {error ? <div className="banner error">{error}</div> : null}
      <div className="muted mono">
        materialised bytes{etag ? ` · etag ${etag.slice(0, 14)}…` : ''}
      </div>
      <textarea readOnly rows={7} value={dav ?? ''} style={{ marginTop: 4 }} />
      <div className="row">
        <button onClick={() => void appendRemote()}>PUT append</button>
        <button onClick={() => setBody(dav ?? '')}>copy to editor below</button>
      </div>
      <textarea
        rows={4}
        placeholder="body for WebDAV PUT (replaces the whole file through the same YDoc)"
        value={body}
        onChange={(event) => setBody(event.target.value)}
        style={{ marginTop: 6 }}
      />
      <div className="row">
        <button className="primary" onClick={() => void put()}>
          PUT /dav
        </button>
        <button
          onClick={async () => {
            try {
              await api.putFile(path, body)
              log('PUT /api/files replaced the content')
              bump()
            } catch (err) {
              setError(String(err))
            }
          }}
        >
          PUT /api/files
        </button>
      </div>
      {documentJson ? (
        <details style={{ marginTop: 8 }}>
          <summary className="muted">/api/document</summary>
          <pre className="mono" style={{ maxHeight: 180, overflow: 'auto' }}>
            {documentJson}
          </pre>
        </details>
      ) : null}
    </div>
  )
}