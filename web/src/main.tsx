import { createRoot } from 'react-dom/client'
import { App } from './App'
import './styles.css'

const container = document.getElementById('root')
if (!container) throw new Error('#root is missing from index.html')

// No <StrictMode>: its double mount would tear down the WebSocket/Y.Doc session that the editor
// is bound to. The session is created once per tab, on purpose.
createRoot(container).render(<App />)