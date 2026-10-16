import { useEffect, useRef } from "react";

/** Where focus lands when the element that opened a dialog is gone (UI-10). */
export const FOCUS_FALLBACK_ID = "url-input";

function currentFocus(): HTMLElement | null {
  const el = document.activeElement;
  return el instanceof HTMLElement && el !== document.body ? el : null;
}

/**
 * Return keyboard focus to whatever opened a dialog once it closes. The dialogs are
 * mounted only while open, so Carbon's `launcherButtonRef` never sees the close and
 * focus would drop to `body`. If the opener has since left the DOM (a row menu item,
 * a toast), focus goes to the URL field.
 */
export function useReturnFocus(open: boolean): void {
  const opener = useRef<HTMLElement | null>(null);
  // Read during render: the dialog's own focus effect runs before ours and would hide the opener.
  if (open && opener.current === null) opener.current = currentFocus();

  useEffect(() => {
    if (!open) return;
    return () => {
      const target = opener.current;
      opener.current = null;
      const usable = target?.isConnected && !(target as HTMLButtonElement).disabled ? target : null;
      (usable ?? document.getElementById(FOCUS_FALLBACK_ID))?.focus();
    };
  }, [open]);
}
