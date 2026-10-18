# MeiSync for Obsidian

Hybrid sync between an Obsidian vault and a self-hosted [MeiSync](../README.md) node:

* open Markdown notes use real YSync/Yjs for character-level realtime editing;
* closed notes, attachments and `.obsidian` config use file-level manifest/ETag sync.

## Build

```bash
npm install
npm run build          # -> dist/
```

`npm run build` writes a ready-to-copy plugin folder to `dist/` (`main.js`, `manifest.json`,
`styles.css`). Copy the contents of `dist/` into
`<vault>/.obsidian/plugins/meisync-obsidian/`, then enable the plugin.

## Configure

1. Set the **Server URL** (for example `http://127.0.0.1:8080`).
2. Enter the MeiSync **username** and **password**.
3. Press **Test connection**. It verifies login and the sync API but does not create any remote
   folders. The first real sync provisions `/private/<username>/obsidian/<vault name>` and
   registers the device.
4. Leave **Real-time Markdown (YSync)** enabled if you want open notes to sync character-by-
   character.
5. Adjust **Remote folder**, exclusions, attachments, `.obsidian` sync, hybrid realtime and
   conflict strategy. Background sync pauses while Settings is open; close it (or press **Sync
   now**) to apply the final remote path. This prevents partial names like `ob` / `obsid` from
   becoming folders.
6. Run **MeiSync: sync now** once and inspect the sync log.

The full reference is in [`docs/obsidian-sync.md`](../docs/obsidian-sync.md).

## Commands

* `MeiSync: sync now`
* `MeiSync: show sync log`
* `MeiSync: reconnect realtime feed`
* `MeiSync: reset sync baseline (dangerous)`

## Notes

* The session token is stored in the plugin data file on this device. Do not sync that file: the
  plugin always excludes its own folder from `.obsidian` configuration sync.
* If you enable `.obsidian` configuration sync, other plugins' `data.json` files are included and
  may contain API keys or tokens. Enable it only for a vault you control.
* Conflict copies are ordinary notes named
  `name.conflict-<device>-<YYYYMMDD-HHMMSS>.ext`.
* Open Markdown notes use YSync `Y.Text("content")` locally. Closed notes, attachments and config
  are whole-file sync. While a note is open, the file-level engine ignores its path to avoid a
  whole-file PUT overwriting a CRDT edit.
* Remote caret decorations are not rendered in Obsidian yet; the web client still sees the
  Obsidian peer's user state.
* The YSync channel itself is verified against a live node (two clients converge, server bytes
  match). The Obsidian Editor bridge still needs a first manual GUI smoke test: open a note and
  look for `ysync ready:` in the sync log.
