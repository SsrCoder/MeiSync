/**
 * Headless end-to-end check of a running MeiSync node.
 *
 * It reuses the *same* provider and markdown codec the browser client uses, so it verifies the
 * wire protocol and the `Y.Text("content")` content model without needing a browser:
 *
 *   1. register / login against `/api`
 *   2. open the same file with two YDocs (two "clients") and check they converge
 *   3. check awareness is relayed between them and dropped when a client leaves
 *   4. check the materialised bytes via WebDAV `GET` match the collaborative text
 *   5. check a WebDAV `PUT` reaches the connected clients (S3/WebDAV -> YSync direction)
 *   6. check the protocol extensions: delete -> `auth denied`, move -> `custom(42, path)`
 *
 * Usage: `npm run e2e -- [--target http://127.0.0.1:8080] [--user e2e]`
 */
import * as Y from 'yjs'
import { Awareness } from 'y-protocols/awareness'
import { MeiSyncApi, ApiError } from '../src/meisync/api'
import { MeiSyncProvider, ysyncUrl } from '../src/meisync/provider'
import { CONTENT_KEY } from '../src/meisync/binding'

// ------------------------------------------------------------------ tiny test harness

let failures = 0
let checks = 0

function check(name: string, ok: boolean, detail = ''): void {
  checks++
  if (ok) {
    console.log(`  \x1b[32mok\x1b[0m   ${name}${detail ? ` — ${detail}` : ''}`)
  } else {
    failures++
    console.log(`  \x1b[31mFAIL\x1b[0m ${name}${detail ? ` — ${detail}` : ''}`)
  }
}

function section(title: string): void {
  console.log(`\n\x1b[1m${title}\x1b[0m`)
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

async function waitFor(predicate: () => boolean, timeoutMs = 5000, label = 'condition'): Promise<boolean> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (predicate()) return true
    await sleep(25)
  }
  console.log(`  \x1b[33m..\x1b[0m timed out waiting for ${label}`)
  return false
}

// ------------------------------------------------------------------ helpers

interface Client {
  doc: Y.Doc
  awareness: Awareness
  provider: MeiSyncProvider
  ytext: Y.Text
}

function connect(target: string, token: string, path: string, name: string): Client {
  const doc = new Y.Doc()
  const awareness = new Awareness(doc)
  awareness.setLocalStateField('user', { name, color: '#6ea8fe' })
  const provider = new MeiSyncProvider({ url: ysyncUrl(target, path), doc, token, awareness })
  return { doc, awareness, provider, ytext: doc.getText(CONTENT_KEY) }
}

function arg(name: string, fallback: string): string {
  const index = process.argv.indexOf(`--${name}`)
  return index !== -1 && process.argv[index + 1] ? process.argv[index + 1] : fallback
}

// ------------------------------------------------------------------ the run

async function main(): Promise<void> {
  const target = arg('target', process.env.MEISYNC_TARGET ?? 'http://127.0.0.1:8080')
  const username = arg('user', `e2e_${Date.now().toString(36)}`)
  const password = 'e2e-secret-123'
  const path = `/private/${username}/notes/e2e.md`

  const api = new MeiSyncApi({ baseUrl: target })

  section(`node ${target}`)
  const health = await api.status()
  check('GET /api/status responds', typeof health === 'object', JSON.stringify(health).slice(0, 80))

  section('accounts')
  let token: string
  try {
    const auth = await api.register(username, password)
    token = auth.token
    check('POST /api/auth/register', true, `user ${auth.user.username}`)
  } catch (error) {
    if (!(error instanceof ApiError)) throw error
    const auth = await api.login(username, password)
    token = auth.token
    check('POST /api/auth/login (already registered)', true)
  }
  api.token = token

  section(`ysync ${path}`)
  await api.ensureCollections(path)
  await api.putFile(path, '')

  const alice = connect(target, token, path, 'alice-client')
  const bob = connect(target, token, path, 'bob-client')
  const aliceOnStatus = alice.provider.on('status', (status) => process.stdout.write(`    alice ysync: ${status}\n`))
  const bobOnStatus = bob.provider.on('status', (status) => process.stdout.write(`    bob   ysync: ${status}\n`))

  await waitFor(() => alice.provider.status === 'connected', 5000, 'alice connected')
  check('client A connected', alice.provider.status === 'connected', alice.provider.status)
  check('client B connected', bob.provider.status === 'connected', bob.provider.status)

  await waitFor(() => alice.provider.synced && bob.provider.synced, 5000, 'initial sync')
  check('both clients completed the initial sync', alice.provider.synced && bob.provider.synced)

  // 2. two clients converge -----------------------------------------------------------
  alice.ytext.insert(0, '# Hello from alice\n\nshared **Y.Text**\n')
  await waitFor(() => bob.ytext.toString() === alice.ytext.toString(), 5000, 'B sees A edit')
  check('A -> B update propagates', bob.ytext.toString() === '# Hello from alice\n\nshared **Y.Text**\n', JSON.stringify(bob.ytext.toString()))

  bob.ytext.insert(bob.ytext.length, '- a line from bob\n')
  await waitFor(() => alice.ytext.toString() === bob.ytext.toString(), 5000, 'A sees B edit')
  check('B -> A update propagates', alice.ytext.toString() === bob.ytext.toString())

  // concurrent edits in different places must merge without a conflict
  alice.ytext.insert(0, 'A: ')
  bob.ytext.insert(bob.ytext.length, 'B: done\n')
  await waitFor(
    () => alice.ytext.toString() === bob.ytext.toString() && alice.ytext.toString().startsWith('A: '),
    5000,
    'concurrent merge',
  )
  const merged = alice.ytext.toString()
  check('concurrent edits merge', alice.ytext.toString() === bob.ytext.toString(), JSON.stringify(merged))

  // 3. awareness ----------------------------------------------------------------------
  alice.awareness.setLocalStateField('cursor', { tname: CONTENT_KEY, item: null, assoc: 0 })
  await waitFor(
    () => Array.from(bob.awareness.getStates().keys()).length === 2,
    5000,
    'awareness relay',
  )
  const bobSees = Array.from(bob.awareness.getStates().entries()).map(([client, state]) => [
    client,
    (state as { user?: { name?: string } }).user?.name,
  ])
  check('B sees both peers in awareness', bobSees.length === 2, JSON.stringify(bobSees))
  check('B knows the remote peer name', bobSees.some(([, name]) => name === 'alice-client'))

  // The node answers a fresh connection with `[sync step1, awareness]` in ONE WebSocket message,
  // so a client that joins late must still learn about the peers that are already inside.
  const dave = connect(target, token, path, 'dave-client')
  await waitFor(() => dave.provider.status === 'connected', 5000, 'dave connected')
  await waitFor(
    () => Array.from(dave.awareness.getStates().keys()).length === 3,
    5000,
    'awareness snapshot for a late joiner',
  )
  const daveSees = Array.from(dave.awareness.getStates().values()).map(
    (state) => (state as { user?: { name?: string } }).user?.name,
  )
  check(
    'late joiner gets the room awareness snapshot',
    daveSees.includes('alice-client') && daveSees.includes('bob-client'),
    JSON.stringify(daveSees),
  )
  dave.provider.destroy()

  // 4. materialisation ----------------------------------------------------------------
  section('WebDAV materialisation')
  const dav = await api.davGet(path)
  check('WebDAV GET matches the collaborative text', dav.text === merged, JSON.stringify(dav.text))
  check('ETag present', Boolean(dav.etag), dav.etag ?? '')

  // 5. HTTP write reaches connected clients -------------------------------------------
  const pushed = '# written over WebDAV\n\nline two\n'
  const putResponse = await api.davPut(path, pushed)
  check('WebDAV PUT accepted', putResponse.ok, `${putResponse.status}`)
  await waitFor(() => alice.ytext.toString() === pushed, 5000, 'PUT reaches ysync clients')
  check('WebDAV PUT -> YSync', alice.ytext.toString() === pushed, JSON.stringify(alice.ytext.toString()))
  await waitFor(() => bob.ytext.toString() === pushed, 5000, 'PUT reaches second client')
  check('WebDAV PUT -> both YSync clients', bob.ytext.toString() === pushed)

  // 6. protocol extensions -----------------------------------------------------------
  section('protocol extensions')
  const movedTo: string[] = []
  alice.provider.on('moved', (next) => movedTo.push(next))
  const renamedPath = `/private/${username}/notes/e2e-renamed.md`
  const moveResponse = await api.davMove(path, renamedPath, target)
  check('WebDAV MOVE accepted', moveResponse.ok, `${moveResponse.status}`)
  await waitFor(() => movedTo.length > 0, 5000, 'custom(42) rename notification')
  check(
    'rename pushes custom(42, path)',
    movedTo[0] === renamedPath,
    JSON.stringify(movedTo[0] ?? null),
  )
  check('renamed file keeps its content', (await api.davGet(renamedPath)).text === pushed)

  const before = await api.davGet(renamedPath)
  check('content unchanged after rename', before.text === pushed)

  alice.provider.destroy()
  bob.provider.destroy()
  aliceOnStatus()
  bobOnStatus()

  // Leaving the room drops the awareness states the connection introduced (§10).
  section('awareness cleanup on close')
  const erin = connect(target, token, renamedPath, 'erin-client')
  await waitFor(() => erin.provider.status === 'connected', 5000, 'erin connected')
  await waitFor(
    () => Array.from(erin.awareness.getStates().keys()).length === 1,
    5000,
    'stale awareness states to disappear',
  )
  check(
    'a late client only sees itself after the others left',
    Array.from(erin.awareness.getStates().keys()).length === 1,
  )
  erin.provider.destroy()

  section('delete notification')
  const carol = connect(target, token, renamedPath, 'carol-client')
  await waitFor(() => carol.provider.status === 'connected', 5000, 'carol connected')
  let denied = ''
  carol.provider.on('denied', (reason) => {
    denied = reason
  })
  await api.deleteFile(renamedPath)
  await waitFor(() => denied !== '', 5000, 'auth denied after delete')
  check('delete pushes auth denied', denied === 'deleted', JSON.stringify(denied))
  carol.provider.destroy()

  section('permission enforcement')
  const strangerName = `e2e_other_${Date.now().toString(36)}`
  const stranger = await api.register(strangerName, password)
  const strangerApi = new MeiSyncApi({ baseUrl: target, token: stranger.token })
  await strangerApi.putFile(`/private/${strangerName}/mine.md`, 'mine')
  try {
    await strangerApi.davGet(path)
    check('private file of another user is not readable', false, 'GET succeeded')
  } catch (error) {
    const status = error instanceof ApiError ? error.status : 0
    check('private file of another user answers 404', status === 404, `status ${status}`)
  }

  console.log(`\n${failures === 0 ? '\x1b[32m' : '\x1b[31m'}${checks - failures}/${checks} checks passed\x1b[0m\n`)
  process.exit(failures === 0 ? 0 : 1)
}

main().catch((error) => {
  console.error('\n\x1b[31me2e run failed\x1b[0m', error)
  process.exit(1)
})