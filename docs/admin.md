# MeiSync Admin Console

The admin console is embedded in the `meisync` binary and served from `/admin`. It is a static
SPA; every operation goes through `/api/admin`, which requires an account with `role = admin`.

## Bootstrapping an administrator

Three ways, in order of preference:

1. Pin the account with environment variables before the first start:

   ```bash
   MEISYNC_ADMIN_USER=admin \
   MEISYNC_ADMIN_PASSWORD='a-long-password' \
   ./target/release/meisync --data ./data --bind 127.0.0.1:8080
   ```

   If `admin` does not exist yet it is created, its `/private/admin` namespace is provisioned and
   the role is set to `admin`. If it already exists, only the role (and the disabled flag) is
   corrected; the password is never silently reset.

2. Let the first registered account become administrator. This is the default on a fresh node
   (`--first-user-admin=true`). It is convenient for a local evaluation but should be disabled
   when registration is reachable from an untrusted network:

   ```bash
   ./target/release/meisync --first-user-admin=false ...
   ```

3. Promote an existing account from another admin console (`Users → Edit → Role`), or set
   `MEISYNC_ADMIN_USER` to that account and restart. On an existing node with accounts but no
   administrator, this is the intended upgrade path; the first-registration rule only applies
   when the node has no users yet.

The admin password is stored in the process environment or CLI; nothing is persisted in plain
text. Accounts are stored with Argon2 hashes. Prefer environment variables over `--admin-password`
so the value is not visible in the process list.

## Authentication

`POST /api/admin/login` uses the same credentials as `/api/auth/login` but:

* rejects non-admin accounts with `403` (their freshly minted token is revoked immediately);
* returns `{ token, expires_at, user, role }`;
* writes an `admin.login` audit entry.

Every other admin endpoint accepts `Authorization: Bearer <token>` only: `?token=` is deliberately
not supported (see `AdminUser`), so an admin token cannot end up in browser history or proxy logs.
A disabled account is rejected by the normal authentication path, so disabling a user revokes
access on the next request, and `update_user` deletes their sessions immediately.

## Roles and safety rails

* `admin` can manage accounts, sessions, devices and any namespace path.
* `user` has no access to `/api/admin/*`.
* You cannot disable or delete your own account from the console.
* You cannot remove or disable the last enabled administrator.
* Deleting an account revokes its sessions and sync devices. With `?purge=true` (the default) the
  account's `/private/<username>` namespace is deleted recursively as well. Files the account
  owns in `/public/...` are kept; ACL grants are attached to file ids, so they remain valid.
* A password reset revokes every active session of that account.

## API reference

All paths are relative to the node origin.

### Session

| method | path | body / notes |
|--------|------|--------------|
| `POST` | `/api/admin/login` | `{"username","password"}` → `{token,expires_at,user,role}` |
| `POST` | `/api/admin/logout` | revokes the bearer token |
| `GET`  | `/api/admin/me` | `{user,role,permissions}` |

### Overview

| method | path | notes |
|--------|------|-------|
| `GET`  | `/api/admin/overview` | `{stats, documents, server, disk, recent_audit, capabilities}` |
| `GET`  | `/api/admin/status` | alias of `overview` |

`stats` contains `users, admins, disabled_users, files, collections, bytes, ydocs,
active_sessions, devices, audit_entries`. `disk` contains the YDoc store size, the SQLite file
size and the total data-directory size (best effort, measured on request).

### Users

| method | path | body |
|--------|------|------|
| `GET`  | `/api/admin/users?search=&limit=&offset=` | `{users,total,limit,offset}` |
| `POST` | `/api/admin/users` | `{"username","password","role?","disabled?"}` |
| `PATCH`| `/api/admin/users/{id}` | `{"role?","disabled?","password?"}` |
| `DELETE`| `/api/admin/users/{id}?purge=true` | deletes the account, sessions, devices and optionally the home namespace |

A user row includes `id, username, role, disabled, created_at, last_login_at, file_count, bytes,
session_count`.

### Sessions and devices

| method | path | notes |
|--------|------|-------|
| `GET`  | `/api/admin/sessions?limit=` | active sessions with username and expiry |
| `DELETE`| `/api/admin/sessions/{id}` | revoke one session by its opaque id |
| `GET`  | `/api/admin/devices?limit=` | Obsidian/sync clients |
| `DELETE`| `/api/admin/devices/{user_id}/{device_id}` | forget a registration (files untouched) |

A session row is `{id, token_prefix, user_id, username, created_at, expires_at}`. The bearer token
itself never leaves the server: `id` is the first 16 hex digits of `sha256(token)` and
`token_prefix` is the masked form (`abcdef…1234`), so the console can list and revoke sessions
without ever putting a live credential into a page, a URL, a log or the clipboard.

### File browser and audit

| method | path | notes |
|--------|------|-------|
| `GET`  | `/api/admin/files?path=/private&depth=1&owner=` | admin view, no permission filtering |
| `GET`  | `/api/admin/file?path=/private/alice/notes.md` | materialised content (`Content-Type`, `ETag`) |
| `DELETE`| `/api/admin/files?path=/private/alice/notes.md` | recursive delete, closes YDoc rooms first |
| `GET`  | `/api/admin/audit?limit=` | newest first |

File deletion through the admin API is intentionally not exposed as an inline editor: the console
is for operations, and editing should go through the file protocols so an editor and YSync stay
consistent.

## Audit log

Audit entries are best effort and never fail the action itself. The table stores `at, actor_id,
actor_name, action, target, detail`. Current actions include:

* `admin.bootstrap`, `admin.login`, `admin.login.failed`, `admin.logout`
* `auth.register`, `auth.login`
* `admin.user.create`, `admin.user.update`, `admin.user.delete`, `admin.user.delete.keep_files`
* `admin.session.revoke`, `admin.device.delete`, `admin.file.delete`
* `sync.device`

The log is exposed read-only through the console and `/api/admin/audit`; retention is manual (it
is a normal SQLite table, so a production deployment can rotate it from cron).
