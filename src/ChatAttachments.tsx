import { createSignal, For, onCleanup, Show } from "solid-js";
import { attachmentUrl, type Attachment } from "./api";
import { acceptFiles, formatSize, isImage } from "./attachments";
import { CloseIcon, FileIcon } from "./icons";
import "./attachments.css";

export const PaperclipIcon = () => (
  <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <path d="M13.2 7.4 8.1 12.5a3.2 3.2 0 0 1-4.6-4.6l5.4-5.4a2.1 2.1 0 0 1 3 3L6.6 10.8a1.05 1.05 0 0 1-1.5-1.5l4.8-4.8" />
  </svg>
);

export interface PendingFile {
  key: number;
  file: File;
  /** An object URL for an image's thumbnail. */
  preview: string | null;
}

let nextKey = 1;
const hasFiles = (e: DragEvent) => !!e.dataTransfer?.types.includes("Files");

/** Files waiting in the composer, added by drop, paste, or the attach button. Drop and paste take files only while `enabled`. */
export function createComposerFiles(onError: (message: string) => void, enabled: () => boolean = () => true) {
  const [items, setItems] = createSignal<PendingFile[]>([]);
  const [dragging, setDragging] = createSignal(false);
  const release = (list: PendingFile[]) => list.forEach((item) => item.preview && URL.revokeObjectURL(item.preview));
  onCleanup(() => release(items()));

  const add = (files: File[]) => {
    if (!files.length) return;
    const { accepted, error } = acceptFiles(
      items().map((item) => item.file),
      files,
    );
    onError(error);
    const added = accepted.map((file) => ({ key: nextKey++, file, preview: isImage(file) ? URL.createObjectURL(file) : null }));
    setItems((list) => [...list, ...added]);
  };

  return {
    items,
    dragging,
    add,
    remove: (key: number) => {
      release(items().filter((item) => item.key === key));
      setItems((list) => list.filter((item) => item.key !== key));
    },
    /** Empties the composer and hands back what was in it, to send or to put back. */
    take: () => {
      const list = items();
      setItems([]);
      return list;
    },
    restore: (list: PendingFile[]) => setItems((current) => [...list, ...current]),
    release,
    onPaste: (e: ClipboardEvent) => {
      const files = Array.from(e.clipboardData?.files ?? []);
      if (!files.length || !enabled()) return;
      e.preventDefault();
      add(files);
    },
    dropTarget: {
      onDragOver: (e: DragEvent) => {
        if (!enabled() || !hasFiles(e)) return;
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
        setDragging(true);
      },
      onDragLeave: (e: DragEvent) => {
        if (!(e.currentTarget as Node).contains(e.relatedTarget as Node | null)) setDragging(false);
      },
      onDrop: (e: DragEvent) => {
        if (!enabled() || !hasFiles(e)) return;
        e.preventDefault();
        setDragging(false);
        add(Array.from(e.dataTransfer?.files ?? []));
      },
    },
  };
}

export type ComposerFiles = ReturnType<typeof createComposerFiles>;

export function AttachButton(props: { onFiles: (files: File[]) => void }) {
  let input!: HTMLInputElement;
  return (
    <>
      <button class="icon-button attach" aria-label="Attach files" title="Attach files" onClick={() => input.click()}>
        <PaperclipIcon />
      </button>
      <input
        ref={input}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          props.onFiles(Array.from(e.currentTarget.files ?? []));
          e.currentTarget.value = "";
        }}
      />
    </>
  );
}

export function PendingFiles(props: { items: PendingFile[]; onRemove: (key: number) => void }) {
  return (
    <Show when={props.items.length}>
      <ul class="attach-list" aria-label="Attachments">
        <For each={props.items}>
          {(item) => (
            <li class={item.preview ? "attach-thumb" : "attach-chip"} title={`${item.file.name}, ${formatSize(item.file.size)}`}>
              <Show
                when={item.preview}
                fallback={
                  <>
                    <FileIcon />
                    <span class="attach-name">{item.file.name}</span>
                    <span class="attach-size">{formatSize(item.file.size)}</span>
                  </>
                }
              >
                {(src) => <img src={src()} alt={item.file.name} />}
              </Show>
              <button class="attach-remove" aria-label={`Remove ${item.file.name}`} title="Remove" onClick={() => props.onRemove(item.key)}>
                <CloseIcon />
              </button>
            </li>
          )}
        </For>
      </ul>
    </Show>
  );
}

export function MessageAttachments(props: { items: Attachment[]; onOpen: (attachment: Attachment) => void }) {
  return (
    <div class="message-attachments">
      <For each={props.items}>
        {(item) => (
          <Show
            when={item.kind === "image"}
            fallback={
              <button class="attach-chip sent" title={`Open ${item.name}`} onClick={() => props.onOpen(item)}>
                <FileIcon />
                <span class="attach-name">{item.name}</span>
                <span class="attach-size">{formatSize(item.size)}</span>
              </button>
            }
          >
            <button class="attach-thumb sent" title={`Open ${item.name}`} onClick={() => props.onOpen(item)}>
              <img src={attachmentUrl(item.id)} alt={item.name} loading="lazy" />
            </button>
          </Show>
        )}
      </For>
    </div>
  );
}

export function DropOverlay() {
  return (
    <div class="drop-overlay" aria-hidden="true">
      <div>
        <PaperclipIcon />
        <span>Drop files to attach</span>
      </div>
    </div>
  );
}
