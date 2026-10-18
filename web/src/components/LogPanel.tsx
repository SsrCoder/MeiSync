import type { LogEntry } from '../Workspace'

export function LogPanel({ logs }: { logs: LogEntry[] }) {
  return (
    <div className="section">
      <h2>Sync log</h2>
      <div className="log">
        {logs.length === 0 ? <div className="muted">no events yet</div> : null}
        {logs.map((entry, index) => (
          <div key={index}>
            <span className="time">{new Date(entry.time).toLocaleTimeString()}</span>
            {entry.message}
          </div>
        ))}
      </div>
    </div>
  )
}