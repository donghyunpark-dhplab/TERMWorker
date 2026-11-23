import type { ITheme } from "@xterm/xterm";

export const TERMINAL_FONT =
  // Windows, then Linux (Ubuntu/Debian packages: fonts-dejavu, fonts-noto-cjk), then macOS.
  '"Cascadia Mono", "Cascadia Code", "JetBrains Mono", Consolas, "D2Coding", "Malgun Gothic", ' +
  '"DejaVu Sans Mono", "Noto Sans Mono", "Noto Sans Mono CJK KR", "NanumGothicCoding", "Menlo", monospace';

export const darkTheme: ITheme = {
  background: "#101217",
  foreground: "#d6dbe4",
  cursor: "#8fb6ff",
  cursorAccent: "#101217",
  selectionBackground: "#2f4a7a",
  selectionInactiveBackground: "#283246",
  black: "#1b1f27",
  red: "#f0727a",
  green: "#8fd18b",
  yellow: "#e8c46b",
  blue: "#6ea8ff",
  magenta: "#c89bf5",
  cyan: "#62cbd8",
  white: "#c9ced8",
  brightBlack: "#5b6475",
  brightRed: "#ff8c93",
  brightGreen: "#a8e3a3",
  brightYellow: "#f5d78e",
  brightBlue: "#94bfff",
  brightMagenta: "#dab6ff",
  brightCyan: "#86dde8",
  brightWhite: "#f2f4f8",
};
