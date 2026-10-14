import { Modal } from "@carbon/react";
import type { DiscardConfirmCopy } from "@/lib/discardConfirm";

interface DiscardModalProps {
  copy: DiscardConfirmCopy | null;
  onConfirm: () => void;
  onCancel: () => void;
}

/** Always-confirm Discard. Danger styling when files will be deleted. */
export function DiscardModal({ copy, onConfirm, onCancel }: DiscardModalProps) {
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
