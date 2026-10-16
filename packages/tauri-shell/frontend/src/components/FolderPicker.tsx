import { useRef, useState } from "react";
import type { SyntheticEvent } from "react";
import { createPortal } from "react-dom";
import { Button, Modal, TextInput } from "@carbon/react";
import { FolderAdd } from "@carbon/icons-react";
import { pickFolder } from "@/tauri/platform";
import { t } from "@/lib/i18n";
import { submitOnEnter } from "@/lib/submitOnEnter";
import { useReturnFocus } from "@/state/useReturnFocus";

interface FolderPickerProps {
  /** Button label (defaults to "Save to…"). */
  label?: string;
  title?: string;
  kind?: "primary" | "secondary" | "tertiary" | "ghost";
  size?: "sm" | "md" | "lg";
  disabled?: boolean;
  onPicked: (path: string) => void;
  onError?: (err: unknown) => void;
  /** How long to wait for the native chooser before offering a typed path. */
  timeoutMs?: number;
}

const DEFAULT_PICK_TIMEOUT_MS = 120_000;

/** Class on the typed-path dialog's wrapper. A host ComposedModal lists it in `selectorsFloatingMenus` so its focus trap lets focus into the dialog. */
export const FOLDER_DIALOG_CLASS = "ph-folder-dialog";

const stop = (e: SyntheticEvent) => e.stopPropagation();

/**
 * Folder chooser: the native portal/zenity/kdialog picker via Rust, with a
 * typed-path fallback for desktops without xdg-desktop-portal.
 */
export function FolderPicker({
  label = t("url.saveTo"),
  title,
  kind = "tertiary",
  size = "md",
  disabled,
  onPicked,
  onError,
  timeoutMs = DEFAULT_PICK_TIMEOUT_MS,
}: FolderPickerProps) {
  const [busy, setBusy] = useState(false);
  const [manualOpen, setManualOpen] = useState(false);
  const [manualPath, setManualPath] = useState("");
  const [noChooser, setNoChooser] = useState(false);

  // Each chooser run gets a token; cancel and timeout bump it so a late reply is ignored (L-61).
  const pickToken = useRef(0);
  useReturnFocus(manualOpen);

  const abandon = () => {
    pickToken.current += 1;
    setBusy(false);
  };

  const choose = async () => {
    const token = ++pickToken.current;
    setBusy(true);
    const timer = window.setTimeout(() => {
      if (pickToken.current !== token) return;
      abandon();
      onError?.(new Error(t("folder.timeout")));
      setNoChooser(false);
      setManualOpen(true);
    }, timeoutMs);
    try {
      const result = await pickFolder(title);
      if (pickToken.current !== token) return;
      if (result.method === "manual") {
        // Neither a portal nor zenity/kdialog: ask for a typed path.
        setNoChooser(true);
        setManualOpen(true);
      } else if (result.path) {
        onPicked(result.path);
      }
    } catch (err) {
      if (pickToken.current !== token) return;
      // The chooser itself failed: fall back to typing a path.
      onError?.(err);
      setNoChooser(false);
      setManualOpen(true);
    } finally {
      window.clearTimeout(timer);
      if (pickToken.current === token) setBusy(false);
    }
  };

  const manualValid = manualPath.trim().startsWith("/");
  const acceptManualPath = () => {
    if (!manualValid) return;
    onPicked(manualPath.trim());
    setManualOpen(false);
  };

  return (
    <>
      <Button
        kind={kind}
        size={size}
        renderIcon={FolderAdd}
        disabled={disabled || busy}
        onClick={() => void choose()}
      >
        {label}
      </Button>
      {busy && (
        <Button kind="ghost" size={size} onClick={abandon} data-testid="folder-picker-cancel">
          {t("folder.cancel")}
        </Button>
      )}
      <Button
        kind="ghost"
        size={size}
        disabled={disabled}
        onClick={() => {
          setNoChooser(false);
          setManualOpen(true);
        }}
        data-testid="folder-picker-manual"
      >
        {t("folder.manual.open")}
      </Button>
      {manualOpen &&
        // Portaled to <body>: inside the probe sheet the open modal's transform makes it the containing
        // block for `position: fixed`, so the dialog and its backdrop were sized to the sheet (UI-8).
        // React events still bubble to the probe sheet through the portal, so the wrapper keeps focus and
        // mouse events to itself. Keydown must reach `document` for Carbon's Escape handling. The sheet's
        // focus trap would also pull focus out of the field; it skips FOLDER_DIALOG_CLASS.
        createPortal(
          <div className={FOLDER_DIALOG_CLASS} onBlur={stop} onFocus={stop} onMouseDown={stop}>
            <Modal
              open
              modalHeading={t("folder.manual.title")}
              primaryButtonText={t("folder.manual.use")}
              secondaryButtonText={t("discard.cancel")}
              primaryButtonDisabled={!manualValid}
              onRequestClose={() => setManualOpen(false)}
              onRequestSubmit={acceptManualPath}
              size="sm"
            >
              <TextInput
                id="manual-folder-path"
                data-modal-primary-focus
                labelText={t("folder.manual.label")}
                helperText={noChooser ? t("folder.manual.noChooser") : t("folder.manual.hint")}
                placeholder="/home/you/Videos"
                value={manualPath}
                invalid={manualPath.length > 0 && !manualValid}
                invalidText={t("folder.manual.invalid")}
                onKeyDown={(e) => submitOnEnter(e, acceptManualPath)}
                onChange={(e) => setManualPath(e.target.value)}
              />
            </Modal>
          </div>,
          document.body,
        )}
    </>
  );
}
