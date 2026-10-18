import { createContext, useContext, useEffect, useMemo, useState } from 'react'
import { useEditor, type Editor } from '@tiptap/react'
import { ApiError, type MeiSyncApi, type PublicUser } from './meisync/api'
import { colorFor, createSession, type Session } from './meisync/session'
import { editorExtensions } from './meisync/schema'
import { yTextBindingExtension } from './meisync/binding'
import { Toolbar } from './components/Toolbar'
import { EditorPane } from './components/EditorPane'
import { FilePanel } from './components/FilePanel'
import { HttpPanel } from './components/HttpPanel'
import { SessionPanel } from './components/SessionPanel'
import { PeersPanel } from './components/PeersPanel'
import { LogPanel } from './components/LogPanel'

export interface LogEntry {
  time: number
  message: string
}

export interface WorkspaceContextValue {
  api: MeiSyncApi
  session: Session
  editor: Editor | null
  user: PublicUser
  path: string
  log: (message: string) => void
  /** Open another namespace path (remounts the session with a fresh Y.Doc). */
  openPath: (path: string) => void
  /** Why the current path cannot be edited, if it cannot. */
  openError: string | null
  /** Bumped by panels that change server state, so other panels can refetch. */
  revision: number
  bump: () => void
}

const WorkspaceContext = createContext<WorkspaceContextValue | null>(null)

export function useWorkspace(): WorkspaceContextValue {
  const value = useContext(WorkspaceContext)
  if (!value) throw new Error('useWorkspace must be used inside <Workspace>')
  return value
}

export function Workspace({
  api,
  auth,
  path,
  seed,
  onPathChange,
}: {
  api: MeiSyncApi
  auth: { token: string; user: PublicUser }
  path: string
  seed: string
  onPathChange: (path: string) => void
}) {
  const [logs, setLogs] = useState<LogEntry[]>([])
  const [revision, setRevision] = useState(0)

  const log = useMemo(
    () => (message: string) =>
      setLogs((previous) => [...previous.slice(-199), { time: Date.now(), message }]),
    [],
  )

  // One Y.Doc + one WebSocket per tab, recreated whenever the tab opens another file.
  const [session] = useState(() =>
    createSession({
      token: auth.token,
      path,
      user: { name: seed ? `${auth.user.username}·${seed}` : auth.user.username, color: colorFor(seed || auth.user.username) },
      onLog: log,
    }),
  )

  useEffect(() => () => session.destroy(), [session])

  const editor = useEditor({
    extensions: [...editorExtensions, yTextBindingExtension(session.binding)],
    autofocus: 'end',
    editorProps: { attributes: { class: 'meisync-editor' } },
  })

  useEffect(() => {
    if (!editor) return
    session.binding.attach(editor.view)
  }, [editor, session])

  // Debug handle for poking at a live session from the browser console.
  useEffect(() => {
    const handle = globalThis as unknown as Record<string, unknown>
    handle.__meisync = { session, editor, binding: session.binding }
    return () => {
      delete handle.__meisync
    }
  }, [session, editor])

  // A YSync handshake for a missing path is rejected with a bare HTTP 404, which the browser
  // cannot explain. Check the control plane first so the failure is visible (and, for a file,
  // just create it — this is a test client, not a production editor).
  const [openError, setOpenError] = useState<string | null>(null)
  useEffect(() => {
    let cancelled = false
    api
      .getFile(path)
      .then((node) => {
        if (cancelled) return
        setOpenError(node.kind === 'collection' ? `${path} is a collection, not a file` : null)
      })
      .catch(async (error) => {
        if (cancelled) return
        if (!(error instanceof ApiError) || error.status !== 404) {
          setOpenError(`cannot open ${path}: ${String(error)}`)
          return
        }
        try {
          await api.ensureCollections(path)
          await api.putFile(path, '')
          if (cancelled) return
          log(`${path} did not exist — created it through /api/files`)
          setOpenError(null)
          setRevision((previous) => previous + 1)
        } catch (createError) {
          if (!cancelled) setOpenError(`cannot create ${path}: ${String(createError)}`)
        }
      })
    return () => {
      cancelled = true
    }
  }, [api, path, log])

  const value = useMemo<WorkspaceContextValue>(
    () => ({
      api,
      session,
      editor,
      user: auth.user,
      path,
      log,
      revision,
      bump: () => setRevision((previous) => previous + 1),
      openPath: onPathChange,
      openError,
    }),
    [api, session, editor, auth.user, path, log, revision, onPathChange, openError],
  )

  if (!path) {
    return (
      <div className="layout">
        <div className="center" style={{ padding: 24 }}>
          <div className="banner">Enter a namespace path in the top bar, e.g. /private/alice/notes/todo.md</div>
        </div>
      </div>
    )
  }

  return (
    <WorkspaceContext.Provider value={value}>
      <div className="layout">
        <aside className="sidebar">
          <FilePanel />
          <HttpPanel />
        </aside>
        <section className="center">
          <Toolbar />
          <EditorPane />
        </section>
        <aside className="sidebar">
          <SessionPanel />
          <PeersPanel />
          <LogPanel logs={logs} />
        </aside>
      </div>
    </WorkspaceContext.Provider>
  )
}