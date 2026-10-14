import { useEffect, useState } from "react";
import { Modal, TextInput } from "@carbon/react";
import type { PromptRequest } from "@/App";
import { t } from "@/lib/i18n";

interface PromptModalProps {
  request: PromptRequest | null;
  onClose: () => void;
}

/** Single-field text prompt (add mirror, per-job bandwidth cap). */
export function PromptModal({ request, onClose }: PromptModalProps) {
  const [value, setValue] = useState("");
  const [touched, setTouched] = useState(false);

  useEffect(() => {
    setValue("");
    setTouched(false);
  }, [request]);

  if (!request) return null;
  const error = request.validate(value);
  const invalid = touched && error != null;

  return (
    <Modal
      open
      size="sm"
      modalHeading={request.title}
      primaryButtonText={request.confirmLabel}
      secondaryButtonText={t("discard.cancel")}
      primaryButtonDisabled={error != null}
      onRequestClose={onClose}
      onRequestSubmit={() => {
        setTouched(true);
        if (error != null) return;
        const submit = request.onSubmit;
        onClose();
        void submit(value);
      }}
    >
      <TextInput
        id="prompt-input"
        labelText={request.label}
        placeholder={request.placeholder}
        value={value}
        invalid={invalid}
        invalidText={error ?? ""}
        onChange={(e) => {
          setValue(e.target.value);
          setTouched(true);
        }}
      />
    </Modal>
  );
}
