import type { KeyboardEvent } from "react";

/**
 * Run `submit` when Enter is pressed in a dialog's text field. Carbon's own
 * `shouldSubmitOnEnter` also fires from the Cancel button, so the field handles it (UI-9).
 * An Enter that confirms an IME composition is left alone.
 */
export function submitOnEnter(e: KeyboardEvent<HTMLElement>, submit: () => void): void {
  if (e.key !== "Enter" || e.shiftKey || e.nativeEvent.isComposing || e.keyCode === 229) return;
  e.preventDefault();
  submit();
}
