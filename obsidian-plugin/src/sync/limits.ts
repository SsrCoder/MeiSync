/**
 * Upload size limits shared by the planner and the sync engine.
 *
 * The plugin has its own "Maximum file size" setting, and the node advertises the body limit it
 * accepts (`--max-body-mb`, 64 MB by default). Uploading above the server limit can only end in
 * `413 Failed to buffer the request body`, and the file would be retried every round, so the
 * effective limit is the smaller of the two.
 */
export const DEFAULT_MAX_FILE_SIZE_MB = 32

/** Effective upload limit in bytes: `min(client limit, server max_body_bytes)`. */
export function effectiveUploadLimit(
  maxFileSizeMb: number,
  serverMaxBodyBytes: number,
): number {
  const clientLimit =
    Math.max(1, Number(maxFileSizeMb) || DEFAULT_MAX_FILE_SIZE_MB) * 1024 * 1024
  const serverLimit = Number(serverMaxBodyBytes)
  if (!Number.isFinite(serverLimit) || serverLimit <= 0) return clientLimit
  return Math.min(clientLimit, serverLimit)
}
