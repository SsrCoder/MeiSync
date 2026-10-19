# Obsidian Sync Plugin

The plugin in [`obsidian-plugin/`](../obsidian-plugin/) syncs a vault against a self-hosted
MeiSync node in **hybrid mode**:

* **Open Markdown notes use real YSync/Yjs.** The editor is bridged to the shared
  `Y.Text("content")` over `ws://.../ysync/<path>`, so edits are character-level and interoperate
  with the web/Tiptap client and other hybrid Obsidian clients.
* **Closed notes, attachments and `.obsidian` config use file-level sync.** The plugin talks a
  small sync API (`/api/sync`) that adds manifests, ETag-conditional writes, a change feed and
  device registration on top of the normal `FileService`.
* While a note is open, the file-level engine is told to ignore its path, so a whole-file PUT can
  never overwrite a CRDT edit.

This mirrors the useful parts of Fast Note Sync (real-time feed, conflict copies, attachment and
config sync, device list) while reusing MeiSync's existing document layer instead of introducing a
second storage model.

## Install

```bash
cd obsidian-plugin
npm install
npm run build
```

`npm run build` writes a ready-to-copy plugin folder to `dist/`:

```text
dist/main.js
dist/manifest.json
dist/styles.css
```

Copy the contents of `dist/` into `<vault>/.obsidian/plugins/meisync-obsidian/`.

Enable **MeiSync** under *Settings → Community plugins*, then open its settings.

## Configure

| setting | default | meaning |
|---------|---------|---------|
| Server URL | – | `http://127.0.0.1:8080`, or the public origin of the node |
| Username / Password | – | used to obtain a session token; the token is stored in the plugin data file |
| Remote folder | `obsidian` | root under `/private/<username>`; the vault name is appended |
| Vault name override | – | use a different remote folder name |
| Device name | platform + vault | shown in the admin console |
| Automatic sync | on | reconcile on a timer and after local changes |
| Sync interval | 30 s | full manifest reconciliation interval |
| Realtime feed | on | file-level WebSocket change feed for immediate reconciliation |
| Real-time Markdown (YSync) | on | open Markdown notes sync through YSync character-by-character; off means purely file-level |
| Sync attachments | on | include binary files; off syncs text-like notes only |
| Sync `.obsidian` | off | include config, themes and snippets. The MeiSync plugin folder, `workspace.json`, `workspace-mobile.json` and cache are always excluded. Other plugins' `data.json` files may contain credentials; review them before enabling |
| Maximum file size | 32 MB | client-side skip limit; the effective limit is the smaller of this and the node's `max_body_bytes` (`--max-body-mb`, 64 MB by default) |
| Exclude patterns | – | one glob per line (`*`, `**`, `?`); `#` starts a comment |
| Conflict strategy | Keep both | `duplicate`, `manual`, `local-wins`, `remote-wins` |
| Initial sync mode | Merge | what to do when a path exists on both sides without a baseline |
| Debug logging | off | log every planned action |

The *Remote path* preview under *Vault* shows the path relative to the account home, for example
`obsidian/My Vault`. The server resolves it as `/private/<username>/<remote folder>/<vault name>`.

Background reconciliation pauses while the settings tab is open, so typing a partial remote folder
name cannot create folders such as `ob/`, `obsid/` or `obsidi/`. The final values are applied when
you close Settings or run **Sync now**. **Test connection** only verifies login and
`/api/sync/info`; it does not create folders.

## Hybrid sync model

### Open Markdown notes: YSync

When a Markdown view opens, the plugin performs a **targeted file-level reconcile** for that note
(`/api/sync/manifest` + conditional content). This guarantees that local text and the server YDoc
agree before a CRDT session starts. It then:

1. Creates a `Y.Doc` and connects a YSync provider to
   `ws://<server>/ysync/private/<user>/<vault>/<note>?token=...`.
2. Waits for the server's `SyncStep2` before binding the editor. (A `SyncStep1` is only a state
   vector request and carries no content.)
3. Applies remote `Y.Text` changes through Obsidian's `Editor.transaction` API, mapping cursors
   through the smallest diff.
4. Pushes local editor changes into `Y.Text` with a local transaction origin, so they are not
   echoed back.
5. On close, persists the final editor text and reconciles the file-level baseline for that path.
   If YSync was offline while editing, the note is left to the normal file-level conflict path
   instead of silently marking the unsynced edits as complete.

The file-level engine skips managed paths in three places: local change events, the file-level
WebSocket feed, and every planner map/action. Other notes, attachments and `.obsidian` config keep
using the file-level path.

Limitations of the current Editor-API bridge:

* remote cursors/awareness decorations are not rendered in Obsidian (the web client still sees
  the Obsidian peer's user state);
* undo/redo isolation for remote changes depends on Obsidian's handling of transaction origins;
* if a remote update and a local edit race during the brief connection window, local edits take
  precedence and a warning is written to the sync log;
* files that are not open, images, PDFs and `.obsidian` config are not character-level
  collaborative.

### Verification status

* The YSync provider is verified against a live node: two independent clients converge
  character-by-character on concurrent edits, and `GET /api/sync/file` materialises exactly the
  converged `Y.Text("content")`.
* The planner, path/exclusion rules and text diff are unit-tested (`npm test`, 28 tests).
* The Obsidian `Editor` bridge (transaction-based apply, cursor mapping, autosave flush) compiles
  and is guarded defensively, but it has not yet been exercised in a real Obsidian GUI. Treat the
  first run as a smoke test: open a note, type in it, and watch the sync log for
  `ysync ready: <path>`.

### File-level rounds

A file-level sync round is:

1. `POST /api/sync/handshake` – sign in if needed, ensure the remote root collection exists,
   register/update the device. **Test connection does not call this endpoint**, so it never
   provisions a folder for a half-typed remote path.
2. `GET /api/sync/manifest` – recursive list of relative paths with `etag`, `version`, `size`,
   `kind` and `content_type`.
3. Scan the local vault (`getAllLoadedFiles`) and apply include/exclude/attachment rules.
4. Diff **baseline ↔ local ↔ remote** with a pure planner
   (`obsidian-plugin/src/sync/planner.ts`).
5. Execute downloads, uploads, deletes, folder creation and conflict handling.
6. Re-read the manifest/local stat and store a new baseline.

The baseline lives in the plugin data file and contains the last synced `etag`, `version`,
`mtime` and `size` per path. Local change detection uses `mtime + size`; when both sides changed,
the engine compares SHA-256 hashes before calling it a conflict.

### Conflict rules

| situation | `duplicate` (default) | `local-wins` | `remote-wins` |
|-----------|-----------------------|--------------|---------------|
| both sides changed | save local copy as `name.conflict-<device>-<stamp>.ext`, upload it, download remote to the original path | upload local (force) | download remote |
| local changed, remote deleted | resurrect local | resurrect local | delete local |
| local deleted, remote changed | restore remote | delete remote | restore remote |
| both created at the same path (no baseline) | conflict copy | upload local | download remote |
| file/folder collision | reported, local side kept | reported | reported |

`manual` behaves like `duplicate` but leaves the conflict copy as a local file for review (it will
be uploaded by a later normal sync if kept). Files that could not be resolved are excluded from the
baseline so the next round retries instead of silently marking them synced.

### Renames and deletes

* Local rename events call `POST /api/sync/move` immediately and remap the baseline prefix. If the
  remote side rejects the move (destination exists, or the path was never synced), the plugin
  falls back to a full reconciliation.
* Local delete + unchanged remote → delete on the remote.
* Remote delete + unchanged local → move the local file to the system trash (not a hard delete).
* Remote rename appears as delete + create in the manifest; the next round downloads the new path
  and removes the old one.

### File-level realtime feed

`GET /api/sync/ws?vault=...&token=...` upgrades to a WebSocket. It is used for file-level paths
(closed notes, attachments, config); open hybrid notes are already updated by YSync. The server
sends:

```json
{"type":"ready","protocol":1,"root":"/private/alice/obsidian/MyVault","seq":42}
{"type":"change","change":{"seq":43,"kind":"updated","file_id":"...","path":"/private/alice/obsidian/MyVault/notes/a.md","etag":"\"...\"","size":12,"collection":false,"updated_at":"...","actor_id":"..."}}
{"type":"resync","reason":"lagged"}
```

The feed covers **every** materialised change, not just file-level writes:

* WebDAV / S3 / `/api/sync/file` writes are announced by `FileService`;
* a YSync edit (an open note on another client) is announced by the YSync session once the update
  has been applied to the shared YDoc. Without that second source, closed copies of the note on
  other clients only noticed the edit on their next manifest poll.

The plugin debounces changes, and a change that arrives while a round is already running queues one
follow-up round instead of being dropped. It also reconciles:

* whenever the feed (re)connects — changes broadcast during the gap are gone, so the manifest is
  the only way to catch up;
* when the Obsidian window regains focus/visibility — background windows have throttled timers and
  a suspended laptop or mobile app can leave a socket dead without a `close` event;
* on the periodic interval, as the final fallback.

A change for a path this client just wrote is not ignored: its own write is suppressed for a couple
of seconds so the echo cannot race the upload baseline, and the notification is re-run after that
window (another client may have edited the same path meanwhile). The feed is advisory: correctness
always comes from the manifest + ETag round.

## API summary

| method | path | notes |
|--------|------|-------|
| `GET`  | `/api/sync/info` | `{service,version,protocol,capabilities,max_body_bytes,conflict_model}` |
| `POST` | `/api/sync/handshake` | `{vault,device_id,name,platform,plugin_version}` → root, user, seq |
| `GET`  | `/api/sync/manifest?vault=` | `If-None-Match` supported; `ETag` is the revision |
| `GET`  | `/api/sync/file?vault=&path=` | `If-None-Match`; returns bytes + `ETag` |
| `PUT`  | `/api/sync/file?vault=&path=` | `If-Match` (update) or `If-None-Match: *` (create) |
| `DELETE`| `/api/sync/file?vault=&path=` | `If-Match`; recursive for collections |
| `POST` | `/api/sync/move` | `{vault,from,to,overwrite}` |
| `POST` | `/api/sync/mkdir` | `{vault,path}`; idempotent |
| `GET`/`POST` | `/api/sync/devices` | list/register this account's devices |
| `GET`  | `/api/sync/ws?vault=&token=` | file-level change feed |
| `WS`   | `/ysync/<path>?token=` | YSync CRDT channel used by open Markdown notes |

All paths are relative to the vault root. The server builds `/private/<username>/<vault>` and
rebases the `vault` value under the caller's own namespace: a leading `/` is stripped, so
`vault=/private/alice` becomes `/private/<you>/private/alice` and never addresses another user.
`..`, backslashes and NUL bytes are rejected with `400`. It never falls back to another user's
namespace.

## Failure behaviour

* Network/server errors mark the status bar as `error`, write the sync log and retry on the next
  interval. Manual sync shows a notice.
* `401` triggers one transparent re-login using the stored credentials, then retries.
* `412` during an upload becomes a conflict resolution rather than a lost update.
* A failed action is removed from the baseline and retried next round; successful actions keep
  their incremental baseline update.
* Files above the size limit or excluded patterns are reported once per round and left untouched.
  The limit is `min(client "Maximum file size", server max_body_bytes)`, so a node configured
  with a smaller body limit never produces endless `413` retries.
* A remote folder is **not** deleted recursively when it contains excluded, oversized or
  failed-to-sync descendants; the delete is skipped and logged so the plugin never destroys data
  it was not tracking.
* If a YSync session does not receive `SyncStep2` within 12 seconds, it is closed and the note
  falls back to file-level sync.
* If the YSync socket was offline while the user edited, closing the note leaves reconciliation to
  the file-level conflict strategy instead of silently marking the stale baseline as synced.

## Development

```bash
cd obsidian-plugin
npm run dev       # watch build
npm run typecheck
npm test          # planner, path/exclusion and text-diff tests
npm run build
```

The planner, path rules and text-diff helpers are pure TypeScript modules with no Obsidian
dependency, so the sync decisions are unit-testable without launching the app. The YSync provider
is exercised against a live node during development (initial sync, local update, remote update).
