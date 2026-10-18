import { useEffect, useState } from 'react'
import { useWorkspace } from '../Workspace'
import type { Peer } from '../meisync/binding'

/** Presence relayed through the room's awareness channel (§10) — never persisted in the YDoc. */
export function PeersPanel() {
  const { session } = useWorkspace()
  const [peers, setPeers] = useState<Peer[]>(session.binding.peers())

  useEffect(() => {
    const awareness = session.provider.awareness
    const update = () => setPeers(session.binding.peers())
    awareness.on('change', update)
    const timer = setInterval(update, 1500)
    return () => {
      awareness.off('change', update)
      clearInterval(timer)
    }
  }, [session])

  return (
    <div className="section">
      <h2>
        Peers <span className="spacer" />
        <span className="muted">{peers.length} connected</span>
      </h2>
      <div className="peers">
        {peers.length === 0 ? <span className="muted">nobody else is in the room</span> : null}
        {peers.map((peer) => (
          <div className="peer" key={peer.clientID}>
            <span className="swatch" style={{ background: peer.color }} />
            <span style={{ flex: 1 }}>{peer.name}</span>
            <span className="muted mono">{peer.cursorIndex == null ? '—' : `@${peer.cursorIndex}`}</span>
          </div>
        ))}
      </div>
    </div>
  )
}