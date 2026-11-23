/** The composer's attachment rules. The backend checks the same limits. */
export const MAX_FILES = 10;
export const MAX_FILE_BYTES = 20 * 1024 * 1024;

/** Formats both Codex and Claude accept as images; anything else goes to the familiar as a file. */
const IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

type FileLike = { name: string; size: number; type: string };

export const isImage = (file: FileLike) => IMAGE_TYPES.has(file.type);

/** Which of `incoming` fit next to `current`, and why any were left out. */
export function acceptFiles<T extends FileLike>(current: readonly FileLike[], incoming: readonly T[]): { accepted: T[]; error: string } {
  const tooBig = incoming.filter((file) => file.size > MAX_FILE_BYTES);
  const fitting = incoming.filter((file) => !tooBig.includes(file));
  const room = Math.max(0, MAX_FILES - current.length);
  const accepted = fitting.slice(0, room);
  const errors: string[] = [];
  if (tooBig.length === 1) errors.push(`${tooBig[0].name} is over 20 MB. Attachments can be up to 20 MB.`);
  if (tooBig.length > 1) errors.push(`${tooBig.length} files are over 20 MB. Attachments can be up to 20 MB.`);
  if (fitting.length > room) errors.push(`Attach up to ${MAX_FILES} files at a time.`);
  return { accepted, error: errors.join(" ") };
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  const mb = bytes / (1024 * 1024);
  return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`;
}
