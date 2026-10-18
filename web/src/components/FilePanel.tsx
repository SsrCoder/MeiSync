import { useCallback, useEffect, useState } from 'react'
import { useWorkspace } from '../Workspace'
import type { FileNode } from '../meisync/api'

export function FilePanel() {
  const { api, path, revision, bump, log, openPath } = useWorkspace()
  const [nodes, setNodes] = useState<FileNode[]>([])
  const [error, setError] = useState<string | null>(null)
  const [newName, setNewName] = useState('')

  const refresh = useCallback(async () => {
    try {
      setNodes(await api.listFiles('/', 'infinity'))
      setError(null)
    } catch (err) {
      setError(String(err))
    }
  }, [api])

  useEffect(() => {
    void refresh()
  }, [refresh, revision])

  const parentOf = (target: string) => {
    const clean = target.replace(/\/+$/, '')
    const index = clean.lastIndexOf('/')
    return index <= 0 ? '/' : clean.slice(0, index)
  }

  const createFile = async () => {
    if (!newName.trim()) return
    const target = `${parentOf(path)}/${newName.trim().replace(/^\//, '')}`
    try {
      // The JSON API needs an existing parent, so make sure the collection chain is there first.
      await api.ensureCollections(target)
      await api.putFile(target, `# ${newName.trim()}\n`)
      log(`created ${target} over /api/files`)
      setNewName('')
      bump()
    } catch (err) {
      setError(String(err))
    }
  }

  const createFolder = async () => {
    if (!newName.trim()) return
    const target = `${parentOf(path)}/${newName.trim().replace(/^\//, '')}`
    try {
      const response = await api.davMkcol(target)
      if (!response.ok) throw new Error(`MKCOL ${target} -> ${response.status} ${await response.text()}`)
      log(`created collection ${target} over WebDAV MKCOL`)
      setNewName('')
      bump()
    } catch (err) {
      setError(String(err))
    }
  }

  const remove = async () => {
    if (!window.confirm(`Delete ${path} recursively?`)) return
    try {
      await api.deleteFile(path)
      log(`deleted ${path}`)
      bump()
    } catch (err) {
      setError(String(err))
    }
  }

  return (
    <div className="section">
      <h2>
        Namespace
        <span className="spacer" />
        <button onClick={() => void refresh()}>refresh</button>
      </h2>
      {error ? <div className="banner error">{error}</div> : null}
      <div className="tree">
        {nodes.map((node) => (
          <button
            key={node.id}
            className={`tree-item${node.path === path ? ' active' : ''}`}
            onClick={() => node.kind === 'file' && openPath(node.path)}
            onDoubleClick={() => {
              if (node.kind !== 'file') return
              const url = new URL(location.href)
              url.searchParams.set('path', node.path)
              window.open(url.toString(), '_blank', 'noopener')
            }}
            title={
              node.kind === 'collection'
                ? 'collection'
                : `${node.content_type} · ${node.sync_mode} sync · v${node.version} · ${node.size} B`
            }
          >
            <span className="kind">{node.kind === 'collection' ? '📁' : '📄'}</span>
            <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis' }}>{node.path}</span>
            <span className="kind">{node.kind === 'collection' ? '' : `${node.size}B`}</span>
          </button>
        ))}
      </div>
      <div className="row" style={{ marginTop: 8 }}>
        <input placeholder="new name" value={newName} onChange={(event) => setNewName(event.target.value)} />
        <button onClick={() => void createFile()}>file</button>
        <button onClick={() => void createFolder()}>dir</button>
        <button onClick={() => void remove()} title="DELETE the open path">
          del
        </button>
      </div>
      <div className="muted mono" style={{ marginTop: 6 }}>
        click a file to open it · double click for a second collaborative tab
      </div>
    </div>
  )
}