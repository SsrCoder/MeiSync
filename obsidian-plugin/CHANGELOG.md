# Changelog

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
