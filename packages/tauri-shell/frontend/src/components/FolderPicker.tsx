import { useState } from "react";
import { Button, Modal, TextInput } from "@carbon/react";
import { FolderAdd } from "@carbon/icons-react";
import { pickFolder } from "@/tauri/platform";
import { t } from "@/lib/i18n";

interface FolderPickerProps {
  /** Button label (defaults to "Save to…"). */
  label?: string;
  title?: string;
  kind?: "primary" | "secondary" | "tertiary" | "ghost";
  size?: "sm" | "md" | "lg";
  disabled?: boolean;
  onPicked: (path: string) => void;
  onError?: (err: unknown) => void;
}

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
}: FolderPickerProps) {
  const [busy, setBusy] = useState(false);
  const [manualOpen, setManualOpen] = useState(false);
  const [manualPath, setManualPath] = useState("");
  const [noChooser, setNoChooser] = useState(false);

  const choose = async () => {
    setBusy(true);
    try {
      const result = await pickFolder(title);
      if (result.method === "manual") {
        // Neither a portal nor zenity/kdialog: ask for a typed path.
        setNoChooser(true);
        setManualOpen(true);
      } else if (result.path) {
        onPicked(result.path);
      }
    } catch (err) {
      // The chooser itself failed: fall back to typing a path.
      onError?.(err);
      setNoChooser(false);
      setManualOpen(true);
    } finally {
      setBusy(false);
    }
  };

  const manualValid = manualPath.trim().startsWith("/");

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
      {manualOpen && (
      <Modal
        open
        modalHeading={t("folder.manual.title")}
        primaryButtonText={t("folder.manual.use")}
        secondaryButtonText={t("discard.cancel")}
        primaryButtonDisabled={!manualValid}
        onRequestClose={() => setManualOpen(false)}
        onRequestSubmit={() => {
          if (!manualValid) return;
          onPicked(manualPath.trim());
          setManualOpen(false);
        }}
        size="sm"
      >
        <TextInput
          id="manual-folder-path"
          labelText={t("folder.manual.label")}
          helperText={noChooser ? t("folder.manual.noChooser") : t("folder.manual.hint")}
          placeholder="/home/you/Videos"
          value={manualPath}
          invalid={manualPath.length > 0 && !manualValid}
          invalidText={t("folder.manual.invalid")}
          onChange={(e) => setManualPath(e.target.value)}
        />
      </Modal>
      )}
    </>
  );
}
