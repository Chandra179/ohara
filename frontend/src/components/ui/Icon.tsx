export type IconName =
  | "activity"
  | "file"
  | "flask"
  | "home"
  | "nodes"
  | "search"
  | "settings";

interface IconProps {
  name: IconName;
  size?: number;
}

const ICON_PATHS: Record<IconName, string> = {
  activity: "M3 12h4l2-7 4 14 2-7h6",
  file: "M6 3h8l4 4v14H6z M14 3v5h5 M9 13h6 M9 17h6",
  flask: "M9 3h6 M11 3v6l-5 8a3 3 0 0 0 2.6 4.5h6.8A3 3 0 0 0 18 17l-5-8V3 M8 15h8",
  home: "m3 10 9-7 9 7v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1z M9 21v-6h6v6",
  nodes: "M7 7h10 M7 17h10 M7 7v10 M17 7v10 M4 4h3v3H4z M17 4h3v3h-3z M4 17h3v3H4z M17 17h3v3h-3z",
  search: "m21 21-4.3-4.3 M10.8 18a7.2 7.2 0 1 1 0-14.4 7.2 7.2 0 0 1 0 14.4Z",
  settings: "M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-1.7 1.7-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.5v.2h-2.4v-.2a1.7 1.7 0 0 0-1-1.5 1.7 1.7 0 0 0-1.9.3l-.1.1L8 17l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.5-1H6.7v-2.4h.2a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.9L8 8.6l1.7-1.7.1.1a1.7 1.7 0 0 0 1.9.3 1.7 1.7 0 0 0 1-1.5v-.2h2.4v.2a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1 1.7 1.7-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.5 1h.2V14h-.2a1.7 1.7 0 0 0-1.5 1Z",
};

export function Icon({ name, size = 18 }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className="icon"
      fill="none"
      height={size}
      viewBox="0 0 24 24"
      width={size}
    >
      <path
        d={ICON_PATHS[name]}
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.8"
      />
    </svg>
  );
}
