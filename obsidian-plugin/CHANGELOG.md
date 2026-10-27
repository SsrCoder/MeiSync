# Changelog

## 0.2.5

- **A rename on one device no longer turns the note into two files on another.** A server MOVE only
  changes `parent_id`/`name`, so the manifest keeps reporting the same `file_id` and ETag under the
  new path — but the plugin never read `file_id`, so a client that was closed (or merely idle)
  during the rename saw "the old path was deleted" and "an unrelated new file appeared". If its
  local copy of the old path had drifted at all (an edit, or just an mtime/size change while the
  app was closed), that became a `local-changed-remote-deleted` conflict: the conflict copy stayed
  at the old path *and* the renamed note was downloaded next to it.
  The baseline now records `file_id` and the planner matches on it, emitting one `move-local`
  action instead of a conflict. A renamed folder is moved once, and its children are remapped
  without individual moves.
- **A transient MOVE failure is retried instead of degrading into delete + create.** Falling back to
  a reconcile uploaded the new path and deleted the old one, which is a *new* YDoc with a fresh
  `file_id` — every other client then saw the same delete + create and lost the note's identity.
  Only a terminal rejection (destination exists, or the path was never synced) falls back.
- The engine no longer reacts to its own `vault.rename` for a remote MOVE as if the user had renamed
  the file, which could schedule a second MOVE for a path the server had already moved.

## 0.2.4

- **The sync indicator now reuses Obsidian's own icon element instead of adding a second icon.**
  On desktop the plugin drives the icon that lives inside the core Sync status bar item (and keeps
  following that item when core Sync is toggled); on mobile it drives the core Sync right-sidebar
  header button. A plugin-owned indicator is only created when core Sync has none at all. If core
  Sync rewrites its own state, the plugin re-asserts the MeiSync state on the next mutation.
- **Idle rounds no longer flip the icon.** `syncing` is only reported once a round actually has
  actions to perform, so the interval timer's "nothing changed" rounds leave the icon (and its
  tooltip) untouched.

## 0.2.3

- **The status bar now reuses Obsidian's own sync status icon.** When the core Sync plugin is
  enabled, the indicator is rendered *inside* its status bar item (`status-bar-item-segment` →
  `status-bar-item-icon sync-status-icon`, core's `sync-small` / `check-small` / `paused` /
  `refresh-cw-off` icons and `mod-working` / `mod-success` / `mod-error` / `mod-spin` colours), so
  the bar keeps a single sync icon; a plugin-owned item is only created when core Sync is off, and
  toggling core Sync is followed live (no restart needed). The state is in the hover tooltip;
  clicking the icon still opens the sync log (the core menu is not triggered).
- On mobile the indicator is a right-sidebar header button with the same icon and colours, which
  is the affordance core Sync uses there (mobile has no status bar).
- **Remote YSync edits no longer move the reading client's scrollbar.** Realtime updates were
  applied with `Editor.transaction()`, which Obsidian always dispatches with
  `scrollIntoView: true`; every incoming edit from another device therefore scrolled each open
  pane back to that pane's cursor, even when the user was reading somewhere else in the note.
  The change is now dispatched on the underlying CodeMirror 6 view with `scrollIntoView: false`,
  so the viewport (and the cursor, which CodeMirror maps through the change itself) stays put.
  The public transaction API is still used as a fallback, with the scroll position restored
  around it.

## 0.2.2

- **Attachment uploads above 2 MB no longer fail with `413 Failed to buffer the request body`.**
  The node's `Bytes`/`Json` extractors were still on axum's 2 MB `DefaultBodyLimit`, so the
  `--max-body-mb` knob (64 MB by default) only ever bounded the raw stream, not the extractor.
  The limit is now applied once, in `server::router`, and covers `/api/files`, `/api/sync/file`
  and every other body-reading route.
- The plugin now honours the node's advertised `max_body_bytes` from the handshake: the effective
  upload limit is `min(Maximum file size, server limit)`, so a node with a smaller body limit
  produces one clear skip message instead of a 413 for the same file on every round.

## 0.2.1

- `npm run build` now writes a ready-to-copy plugin folder to `dist/` (`main.js`,
  `manifest.json`, `styles.css`) instead of a loose `main.js` next to the sources.
- **Remote YSync edits now reach other clients immediately.** The server publishes a file-level
  change event when an open note's YDoc update is materialised, so a closed copy of that note on
  another device no longer waits for its next manifest poll (or a manual **Sync now**).
- The change feed is reconciled on every (re)connect, not only when the last sync was more than
  five seconds ago; changes broadcast during a reconnect gap are recovered from the manifest.
- Regaining window focus/visibility triggers a catch-up round (and revives a dead feed), because
  background windows throttle timers and a suspended app can leave a WebSocket dead.
- A change event that arrives during a running round now queues one follow-up round instead of
  being dropped, and an event for a path this client just wrote is re-run after the suppression
  window instead of being discarded.
- The server no longer treats an empty `SyncStep2` (an up-to-date client answering our
  `SyncStep1`) as a write, so it cannot publish bogus change events or bump the version.

## 0.2.0

- **Hybrid realtime mode.** Open Markdown notes now sync character-by-character through YSync /
  `Y.Text("content")`; closed notes, attachments and `.obsidian` config remain file-level.
- A targeted file-level reconcile runs before each YSync session, and the file-level engine ignores
  managed note paths so whole-file PUTs cannot overwrite CRDT edits.
- Baseline refresh on session close/rename avoids conflict copies after realtime editing.
- Added a YSync provider with correct `SyncStep2` gating, reconnect/awareness handling, and a
  YSync-session manager for Obsidian editors (minimal-diff apply, cursor mapping).
- Added text-diff unit tests. Verified against a live node that two YSync clients converge on
  concurrent character-level edits and that `GET /api/sync/file` materialises the converged text.
- The Obsidian `Editor` bridge still needs a first manual GUI smoke test (open a note, watch for
  `ysync ready:` in the sync log).

## 0.1.1

- Pause background reconciliation while the settings tab is open. Typing a remote folder name no
  longer creates intermediate remote folders (`ob/`, `obsid/`, `obsidi/`, …); the final path is
  applied when Settings is closed or **Sync now** runs.
- **Test connection** now only verifies login and `/api/sync/info`; it no longer provisions the
  remote folder or registers the device. The first real sync does that.
- Avoid reconnecting the realtime feed on every debounced settings change.
- Better handling of hidden `.obsidian` files and file/folder collisions.
- Guard remote folder deletion when excluded, oversized or failed children are present.

## 0.1.0

- Initial MeiSync Obsidian client: manifest reconciliation, ETag conflicts, attachments,
  `.obsidian` config sync, realtime WebSocket feed and device registration.
