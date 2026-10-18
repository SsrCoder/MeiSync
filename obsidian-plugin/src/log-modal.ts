import { App, Modal } from 'obsidian'
import type { SyncLog } from './log'
import type { LogEntry } from './types'

export class SyncLogModal extends Modal {
  private unsubscribe: (() => void) | null = null
  private listEl: HTMLElement | null = null

  constructor(app: App, private readonly log: SyncLog) {
    super(app)
  }

  override onOpen(): void {
    this.setTitle('MeiSync sync log')
    this.modalEl.addClass('meisync-log-modal')
    const { contentEl } = this
    contentEl.empty()

    const toolbar = contentEl.createDiv({ cls: 'meisync-log-toolbar' })
    toolbar.createSpan({ cls: 'meisync-log-hint', text: `${this.log.all().length} entries` })
    const copy = toolbar.createEl('button', { text: 'Copy' })
    copy.addEventListener('click', async () => {
      try {
        await navigator.clipboard.writeText(this.log.asText())
        copy.setText('Copied')
      } catch {
        copy.setText('Copy failed')
      }
      window.setTimeout(() => copy.setText('Copy'), 1200)
    })
    const clear = toolbar.createEl('button', { text: 'Clear' })
    clear.addEventListener('click', () => {
      this.log.clear()
      this.render()
    })
    const close = toolbar.createEl('button', { text: 'Close', cls: 'mod-cta' })
    close.addEventListener('click', () => this.close())

    this.listEl = contentEl.createDiv({ cls: 'meisync-log-list' })
    this.render()
    this.unsubscribe = this.log.onChange(() => this.render())
  }

  override onClose(): void {
    if (this.unsubscribe) this.unsubscribe()
    this.unsubscribe = null
    this.contentEl.empty()
  }

  private render(): void {
    if (!this.listEl) return
    this.listEl.empty()
    const entries = this.log.all()
    if (!entries.length) {
      this.listEl.createDiv({ cls: 'meisync-log-empty', text: 'No log entries yet. Run “MeiSync: sync now” to start.' })
      return
    }
    for (const entry of entries) this.listEl.appendChild(renderEntry(entry))
    this.listEl.scrollTop = this.listEl.scrollHeight
  }
}

function renderEntry(entry: LogEntry): HTMLElement {
  const row = document.createElement('div')
  row.addClass('meisync-log-entry', `meisync-log-${entry.level}`)
  const time = row.createSpan({ cls: 'meisync-log-time' })
  time.setText(new Date(entry.at).toLocaleTimeString())
  const level = row.createSpan({ cls: 'meisync-log-level' })
  level.setText(entry.level)
  const message = row.createSpan({ cls: 'meisync-log-message' })
  message.setText(entry.message)
  return row
}
