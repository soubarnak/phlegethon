import { useCallback, useEffect, useState } from "react";

export type ThemeName = "g10" | "g100";

const THEME_KEY = "phlegethon-theme";

function readStored(): ThemeName | null {
  try {
    const raw = window.localStorage.getItem(THEME_KEY);
    if (raw === "g10" || raw === "g100") return raw;
    // Legacy values from the previous shell.
    if (raw === "light") return "g10";
    if (raw === "dark") return "g100";
  } catch {
    /* no storage */
  }
  return null;
}

function systemTheme(): ThemeName {
  try {
    return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "g100" : "g10";
  } catch {
    return "g10";
  }
}

export function useTheme(): [ThemeName, () => void, (t: ThemeName) => void] {
  const [theme, setThemeState] = useState<ThemeName>(() => readStored() ?? systemTheme());

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    try {
      window.localStorage.setItem(THEME_KEY, theme);
    } catch {
      /* ignore */
    }
  }, [theme]);

  const setTheme = useCallback((t: ThemeName) => setThemeState(t), []);
  const toggle = useCallback(() => setThemeState((t) => (t === "g10" ? "g100" : "g10")), []);
  return [theme, toggle, setTheme];
}
