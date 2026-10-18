import { App, Notice, PluginSettingTab, Setting } from 'obsidian'
import type MeiSyncPlugin from './main'
import { DEFAULT_SETTINGS, type ConflictStrategy, type InitialSyncMode } from './types'
import { remoteVaultPath } from './sync/paths'

function debounce<T extends (...args: never[]) => void>(fn: T, wait = 500): T {
  let timer: number | null = null
  return ((...args: never[]) => {
    if (timer !== null) window.clearTimeout(timer)
    timer = window.setTimeout(() => {
      timer = null
      fn(...args)
    }, wait)
  }) as T
}

export class MeiSyncSettingTab extends PluginSettingTab {
  constructor(app: App, private readonly plugin: MeiSyncPlugin) {
    super(app, plugin)
  }

  override hide(): void {
    this.plugin.onSettingsClosed()
  }

  override display(): void {
    // `display()` is called by Obsidian when this tab is opened (the async button handlers below
    // no longer re-render the whole tab). Pausing here prevents partial remote paths from being
    // provisioned while the user is still typing.
    this.plugin.onSettingsOpened()
    const { containerEl } = this
    containerEl.empty()
    containerEl.addClass('meisync-settings')
    const settings = this.plugin.settings
    let refreshPreview: () => void = () => {}
    // Text fields update the in-memory setting immediately (so “Test connection” and “Sync now”
    // see what is currently typed) while persistence / engine reconfiguration stays debounced.
    const saveSoon = debounce(() => {
      void this.plugin.onSettingsChanged()
    }, 400)
    const commit = (apply: (value: string) => void) => (value: string) => {
      apply(value)
      saveSoon()
    }

    containerEl.createEl('h2', { text: 'MeiSync' })
    containerEl.createEl('p', {
      cls: 'setting-item-description',
      text: 'Sync this vault with a self-hosted MeiSync node. The node stores every note as a YDoc and exposes the same files through WebDAV and S3, so other tools can read the vault too.',
    })

    containerEl.createEl('h3', { text: 'Connection' })
    new Setting(containerEl)
      .setName('Server URL')
      .setDesc('Base URL of your MeiSync node, for example http://127.0.0.1:8080.')
      .addText((text) =>
        text
          .setPlaceholder('http://127.0.0.1:8080')
          .setValue(settings.serverUrl)
          .onChange(commit((value) => {
            settings.serverUrl = value.trim()
          })),
      )

    new Setting(containerEl)
      .setName('Username')
      .setDesc('Your MeiSync account. The first account on a fresh node becomes administrator.')
      .addText((text) =>
        text
          .setPlaceholder('alice')
          .setValue(settings.username)
          .onChange(commit((value) => {
            settings.username = value.trim()
          })),
      )

    new Setting(containerEl)
      .setName('Password')
      .setDesc('Used to obtain a session token. It is stored in the plugin data file on this device.')
      .addText((text) => {
        text.inputEl.type = 'password'
        text
          .setPlaceholder('••••••••')
          .setValue(settings.password)
          .onChange(commit((value) => {
            settings.password = value
          }))
      })

    // Never echo any part of the token: it is a live bearer credential and this pane ends up in
    // screenshots and support threads.
    const sessionDesc = () =>
      settings.token
        ? 'Signed in. The session token is stored in the plugin data file on this device.'
        : 'Not signed in yet. Enter credentials and press “Test connection”.'
    let refreshSession: () => void = () => {}
    const sessionSetting = new Setting(containerEl)
      .setName('Session')
      .setDesc(sessionDesc())
    refreshSession = () => sessionSetting.setDesc(sessionDesc())
    sessionSetting.addButton((button) =>
      button.setButtonText('Test connection').onClick(async () => {
        button.setDisabled(true)
        button.setButtonText('Testing…')
        try {
          const result = await this.plugin.testConnection()
          new Notice(
            `MeiSync connected: ${result.server}, user ${result.user}\nRemote root: ${result.root}\nKnown devices: ${result.devices}`,
            7000,
          )
        } catch (error) {
          new Notice(`MeiSync connection failed: ${error instanceof Error ? error.message : String(error)}`, 9000)
        } finally {
          button.setDisabled(false)
          button.setButtonText('Test connection')
          refreshSession()
        }
      }),
    )
    sessionSetting.addButton((button) =>
      button.setButtonText('Sign out').onClick(async () => {
        settings.token = ''
        settings.password = ''
        await this.plugin.onSettingsChanged()
        new Notice('MeiSync: signed out on this device')
        refreshSession()
      }),
    )

    containerEl.createEl('h3', { text: 'Vault' })
    new Setting(containerEl)
      .setName('Remote folder')
      .setDesc('Folder under /private/<username> where this vault lives. The vault name is appended automatically. Background sync pauses while Settings is open, so typing a partial name does not create folders.')
      .addText((text) =>
        text
          .setPlaceholder('obsidian')
          .setValue(settings.remoteRoot)
          .onChange(commit((value) => {
            settings.remoteRoot = value.trim()
            refreshPreview()
          })),
      )

    const previewSetting = new Setting(containerEl)
      .setName('Remote path')
      .setDesc(`Relative to /private/<username>: ${remoteVaultPathSafe(this.plugin)}`)
      .addExtraButton((button) => button.setIcon('copy').setTooltip('Copy path').onClick(async () => {
        try {
          await navigator.clipboard.writeText(remoteVaultPathSafe(this.plugin))
          new Notice('Remote path copied')
        } catch {
          new Notice('MeiSync: could not access the clipboard')
        }
      }))
    refreshPreview = () =>
      previewSetting.setDesc(`Relative to /private/<username>: ${remoteVaultPathSafe(this.plugin)}`)

    new Setting(containerEl)
      .setName('Vault name override')
      .setDesc('Use this if two vaults have the same name, or if you want a different remote folder name.')
      .addText((text) =>
        text
          .setPlaceholder(this.app.vault.getName())
          .setValue(settings.vaultNameOverride)
          .onChange(commit((value) => {
            settings.vaultNameOverride = value.trim()
            refreshPreview()
          })),
      )

    new Setting(containerEl)
      .setName('Device name')
      .setDesc('Shown in the admin console. Defaults to the platform and vault name.')
      .addText((text) =>
        text
          .setPlaceholder('MacBook · My Vault')
          .setValue(settings.deviceName)
          .onChange(commit((value) => {
            settings.deviceName = value.trim()
          })),
      )

    containerEl.createEl('h3', { text: 'Sync' })
    new Setting(containerEl)
      .setName('Automatic sync')
      .setDesc('Reconcile the vault on a timer and after local file events.')
      .addToggle((toggle) =>
        toggle.setValue(settings.autoSync).onChange(async (value) => {
          settings.autoSync = value
          await this.plugin.onSettingsChanged()
        }),
      )

    new Setting(containerEl)
      .setName('Sync interval (seconds)')
      .setDesc('How often a full manifest reconciliation runs. Minimum 10 seconds.')
      .addText((text) => {
        text.inputEl.type = 'number'
        text.inputEl.min = '10'
        text.inputEl.max = '3600'
        text
          .setValue(String(settings.syncIntervalSeconds))
          .onChange(commit((value) => {
            const parsed = Number(value)
            settings.syncIntervalSeconds = Number.isFinite(parsed) ? Math.max(10, Math.min(3600, parsed)) : 30
          }))
      })

    new Setting(containerEl)
      .setName('Realtime feed')
      .setDesc('Keep a WebSocket open so changes from other devices trigger a file-level sync.')
      .addToggle((toggle) =>
        toggle.setValue(settings.realtime).onChange(async (value) => {
          settings.realtime = value
          await this.plugin.onSettingsChanged()
        }),
      )

    new Setting(containerEl)
      .setName('Real-time Markdown editing (YSync)')
      .setDesc(
        'Open Markdown notes sync character-by-character through YSync. Attachments, config and closed notes keep using file-level sync. While a note is open, the file-level engine ignores it.',
      )
      .addToggle((toggle) =>
        toggle.setValue(settings.hybridRealtime).onChange(async (value) => {
          settings.hybridRealtime = value
          await this.plugin.onSettingsChanged()
        }),
      )

    new Setting(containerEl)
      .setName('Sync attachments')
      .setDesc('Include images, PDFs and other binary files. When off, only text-like notes are synced.')
      .addToggle((toggle) =>
        toggle.setValue(settings.syncAttachments).onChange(async (value) => {
          settings.syncAttachments = value
          await this.plugin.onSettingsChanged()
        }),
      )

    new Setting(containerEl)
      .setName('Sync .obsidian configuration')
      .setDesc('Include .obsidian snippets, themes and plugin settings. The MeiSync token and workspace state are always excluded.')
      .addToggle((toggle) =>
        toggle.setValue(settings.syncConfig).onChange(async (value) => {
          settings.syncConfig = value
          await this.plugin.onSettingsChanged()
        }),
      )

    new Setting(containerEl)
      .setName('Maximum file size (MB)')
      .setDesc('Files above this limit are skipped and reported in the sync log. The server body limit still applies.')
      .addText((text) => {
        text.inputEl.type = 'number'
        text.inputEl.min = '1'
        text
          .setValue(String(settings.maxFileSizeMb))
          .onChange(commit((value) => {
            const parsed = Number(value)
            settings.maxFileSizeMb = Number.isFinite(parsed) ? Math.max(1, parsed) : DEFAULT_SETTINGS.maxFileSizeMb
          }))
      })

    new Setting(containerEl)
      .setName('Exclude patterns')
      .setDesc('One glob per line, matched against vault-relative paths. Supports * and **. Lines starting with # are comments.')
      .addTextArea((area) => {
        area.inputEl.rows = 5
        area.inputEl.addClass('meisync-textarea')
        area
          .setPlaceholder('templates/**\nDaily/private/**\n*.tmp')
          .setValue(settings.excludePatterns)
          .onChange(commit((value) => {
            settings.excludePatterns = value
          }))
      })

    new Setting(containerEl)
      .setName('Conflict strategy')
      .setDesc('What to do when both sides changed, or when a file was deleted on one side and edited on the other.')
      .addDropdown((dropdown) =>
        dropdown
          .addOption('duplicate', 'Keep both (conflict copy)')
          .addOption('manual', 'Keep a copy for manual review')
          .addOption('local-wins', 'This device wins')
          .addOption('remote-wins', 'Remote wins')
          .setValue(settings.conflictStrategy)
          .onChange(async (value) => {
            settings.conflictStrategy = value as ConflictStrategy
            await this.plugin.onSettingsChanged()
          }),
      )

    new Setting(containerEl)
      .setName('Initial sync mode')
      .setDesc('Used when a path exists on both sides without a previous baseline. “Merge” is the safest default.')
      .addDropdown((dropdown) =>
        dropdown
          .addOption('merge', 'Merge (conflict copies)')
          .addOption('prefer-local', 'Prefer this device')
          .addOption('prefer-remote', 'Prefer remote')
          .setValue(settings.initialSync)
          .onChange(async (value) => {
            settings.initialSync = value as InitialSyncMode
            await this.plugin.onSettingsChanged()
          }),
      )

    new Setting(containerEl)
      .setName('Debug logging')
      .setDesc('Log every planned action. Useful when diagnosing a sync loop.')
      .addToggle((toggle) =>
        toggle.setValue(settings.debug).onChange(async (value) => {
          settings.debug = value
          await this.plugin.onSettingsChanged()
        }),
      )

    containerEl.createEl('h3', { text: 'Maintenance' })
    new Setting(containerEl)
      .setName('Sync now')
      .setDesc('Run a full reconciliation immediately.')
      .addButton((button) =>
        button.setButtonText('Sync now').setCta().onClick(async () => {
          button.setDisabled(true)
          try {
            await this.plugin.syncNow()
          } finally {
            button.setDisabled(false)
          }
        }),
      )
      .addButton((button) =>
        button.setButtonText('Show log').onClick(() => this.plugin.showLog()),
      )

    new Setting(containerEl)
      .setName('Reset sync baseline')
      .setDesc('Forget which files were last synced. The next sync treats local and remote files as new and may create conflict copies.')
      .addButton((button) =>
        button.setWarning().setButtonText('Reset baseline').onClick(async () => {
          if (!window.confirm('Reset the MeiSync baseline for this vault? The next sync will reconcile from scratch and may create conflict copies.')) return
          await this.plugin.resetBaseline()
        }),
      )

    const last = this.plugin.state.lastSyncAt
    containerEl.createEl('p', {
      cls: 'setting-item-description meisync-settings-foot',
      text: last
        ? `Last successful sync: ${new Date(last).toLocaleString()} · device ${this.plugin.state.deviceId.slice(0, 8)}…`
        : `Not synced yet · device ${this.plugin.state.deviceId.slice(0, 8)}…`,
    })
  }
}

function remoteVaultPathSafe(plugin: MeiSyncPlugin): string {
  try {
    return remoteVaultPath(plugin.settings, plugin.app.vault.getName())
  } catch (error) {
    return `Invalid configuration: ${error instanceof Error ? error.message : String(error)}`
  }
}
