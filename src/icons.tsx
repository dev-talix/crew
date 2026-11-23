import type { JSX } from "@solidjs/web";

function Icon(props: { children: JSX.Element; size?: number }) {
  return (
    <svg
      width={props.size ?? 16}
      height={props.size ?? 16}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {props.children}
    </svg>
  );
}

export const PlusIcon = () => (
  <Icon>
    <path d="M8 3v10M3 8h10" />
  </Icon>
);

export const GearIcon = () => (
  <Icon>
    <path d="M12.90 8.06 L14.30 9.10 L13.24 11.68 L11.51 11.42 L11.42 11.51 L11.68 13.24 L9.10 14.30 L8.06 12.90 L7.94 12.90 L6.90 14.30 L4.32 13.24 L4.58 11.51 L4.49 11.42 L2.76 11.68 L1.70 9.10 L3.10 8.06 L3.10 7.94 L1.70 6.90 L2.76 4.32 L4.49 4.58 L4.58 4.49 L4.32 2.76 L6.90 1.70 L7.94 3.10 L8.06 3.10 L9.10 1.70 L11.68 2.76 L11.42 4.49 L11.51 4.58 L13.24 4.32 L14.30 6.90 L12.90 7.94 Z" />
    <circle cx="8" cy="8" r="2" />
  </Icon>
);

export const FolderIcon = () => (
  <Icon>
    <path d="M2 4.5c0-.8.7-1.5 1.5-1.5h2.6l1.4 1.5h5c.8 0 1.5.7 1.5 1.5v5.5c0 .8-.7 1.5-1.5 1.5h-9c-.8 0-1.5-.7-1.5-1.5z" />
  </Icon>
);

export const ShuffleIcon = () => (
  <Icon>
    <path d="M2 4.5h2.5c3 0 4 7 7 7H14M2 11.5h2.5c1.2 0 2-.9 2.7-2M11.5 4.5H14M12 2.5l2 2-2 2M12 9.5l2 2-2 2" />
  </Icon>
);

export const SendIcon = () => (
  <Icon>
    <path d="M8 13V3.5M3.8 7.5 8 3.3l4.2 4.2" />
  </Icon>
);

export const StopIcon = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
    <rect x="3" y="3" width="8" height="8" rx="1.5" fill="currentColor" />
  </svg>
);

export const TerminalIcon = () => (
  <Icon size={14}>
    <path d="M3 5l3 3-3 3M8 11.5h5" />
  </Icon>
);

export const HomeIcon = () => (
  <Icon>
    <path d="M2.5 7.2 8 2.8l5.5 4.4V13a.5.5 0 0 1-.5.5H10V10H6v3.5H3a.5.5 0 0 1-.5-.5z" />
  </Icon>
);

export const ClockIcon = () => (
  <Icon>
    <circle cx="8" cy="8" r="5.8" />
    <path d="M8 4.6V8l2.3 1.5" />
  </Icon>
);

export const MicIcon = () => (
  <Icon>
    <rect x="5.8" y="1.8" width="4.4" height="7.6" rx="2.2" />
    <path d="M3.4 7.6a4.6 4.6 0 0 0 9.2 0M8 12.2v2" />
  </Icon>
);

export const ComposeIcon = () => (
  <Icon>
    <path d="M13 8.5V12a1.5 1.5 0 0 1-1.5 1.5h-7A1.5 1.5 0 0 1 3 12V5A1.5 1.5 0 0 1 4.5 3.5H8M11.3 2.7l2 2L8.5 9.5 6 10l.5-2.5z" />
  </Icon>
);

export const PaletteIcon = () => (
  <Icon>
    <path d="M8 1.8a6.2 6.2 0 0 0 0 12.4c1 0 1.4-.6 1.4-1.3 0-.9-.8-1.2-.8-2 0-.7.6-1.2 1.4-1.2h1.6a2.6 2.6 0 0 0 2.6-2.6C14.2 4.3 11.4 1.8 8 1.8z" />
    <circle cx="5" cy="7" r=".9" fill="currentColor" stroke="none" />
    <circle cx="7.6" cy="4.6" r=".9" fill="currentColor" stroke="none" />
    <circle cx="10.6" cy="5.4" r=".9" fill="currentColor" stroke="none" />
  </Icon>
);

export const KeyIcon = () => (
  <Icon>
    <circle cx="5.5" cy="10.5" r="3" />
    <path d="M7.6 8.4 13.5 2.5M11.5 4.5l1.8 1.8M9.8 6.2l1.4 1.4" />
  </Icon>
);

export const CardIcon = () => (
  <Icon>
    <rect x="1.8" y="3.5" width="12.4" height="9" rx="1.6" />
    <path d="M1.8 6.5h12.4M4.5 10h2.5" />
  </Icon>
);

export const VaultIcon = () => (
  <Icon>
    <rect x="2" y="2.5" width="12" height="11" rx="1.8" />
    <circle cx="8" cy="8" r="2.4" />
    <path d="M8 5.6v-.8M8 11.2v-.8M10.4 8h.8M4.8 8h.8M4 13.5v1M12 13.5v1" />
  </Icon>
);

export const LockIcon = () => (
  <Icon size={14}>
    <rect x="3.5" y="7" width="9" height="6.5" rx="1.5" />
    <path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" />
  </Icon>
);

export const LockOpenIcon = () => (
  <Icon size={14}>
    <rect x="3.5" y="7" width="9" height="6.5" rx="1.5" />
    <path d="M5.5 7V5a2.5 2.5 0 0 1 4.9-.7" />
  </Icon>
);

export const CopyIcon = () => (
  <Icon size={13}>
    <rect x="5.5" y="5.5" width="8" height="8" rx="1.5" />
    <path d="M10.5 5.5V4a1.5 1.5 0 0 0-1.5-1.5H4A1.5 1.5 0 0 0 2.5 4v5A1.5 1.5 0 0 0 4 10.5h1.5" />
  </Icon>
);

export const CheckIcon = () => (
  <Icon size={13}>
    <path d="M3 8.5 6.5 12 13 4.5" />
  </Icon>
);

export const ChevronIcon = () => (
  <Icon size={12}>
    <path d="M6 4l4 4-4 4" />
  </Icon>
);

export const EditIcon = () => (
  <Icon size={14}>
    <path d="M10.5 2.5l3 3L6 13H3v-3z" />
  </Icon>
);

export const FileIcon = () => (
  <Icon size={14}>
    <path d="M4 1.8h5l3 3V14a.6.6 0 0 1-.6.6H4.6A.6.6 0 0 1 4 14z M9 1.8V5h3" />
  </Icon>
);

export const SearchIcon = () => (
  <Icon size={14}>
    <circle cx="7" cy="7" r="4.2" />
    <path d="M10.2 10.2 13.5 13.5" />
  </Icon>
);

export const GlobeIcon = () => (
  <Icon size={14}>
    <circle cx="8" cy="8" r="5.8" />
    <path d="M2.2 8h11.6M8 2.2c1.6 1.6 2.4 3.5 2.4 5.8S9.6 12.2 8 13.8C6.4 12.2 5.6 10.3 5.6 8S6.4 3.8 8 2.2" />
  </Icon>
);

export const WrenchIcon = () => (
  <Icon size={14}>
    <path d="M9.8 2.4a3.4 3.4 0 0 0-3.9 4.6L2.4 10.5a1.4 1.4 0 0 0 2 2L7.9 9a3.4 3.4 0 0 0 4.6-3.9L10.6 7 9 6.9 8.9 5.3z" />
  </Icon>
);

export const CloseIcon = () => (
  <Icon>
    <path d="M4 4l8 8M12 4l-8 8" />
  </Icon>
);

export const ExpandIcon = () => (
  <Icon>
    <path d="M9.5 2.5h4v4M13.5 2.5 9 7M6.5 13.5h-4v-4M2.5 13.5 7 9" />
  </Icon>
);

