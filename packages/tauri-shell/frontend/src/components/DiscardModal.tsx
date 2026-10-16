import { Modal } from "@carbon/react";
import type { DiscardConfirmCopy } from "@/lib/discardConfirm";
import { useReturnFocus } from "@/state/useReturnFocus";

interface DiscardModalProps {
  copy: DiscardConfirmCopy | null;
  onConfirm: () => void;
  onCancel: () => void;
}

/** Always-confirm Discard. Danger styling when files will be deleted. */
export function DiscardModal({ copy, onConfirm, onCancel }: DiscardModalProps) {
  useReturnFocus(copy != null);
  if (!copy) return null;
  return (
    <Modal
      open
      danger={copy.deletesFiles}
      size="sm"
      modalHeading={copy.title}
      primaryButtonText={copy.confirmLabel}
      secondaryButtonText={copy.cancelLabel}
      onRequestClose={onCancel}
      onRequestSubmit={onConfirm}
    >
      <p style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>{copy.body}</p>
    </Modal>
  );
}
