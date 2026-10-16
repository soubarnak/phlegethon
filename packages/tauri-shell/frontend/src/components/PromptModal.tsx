import { useEffect, useState } from "react";
import { Modal, TextInput } from "@carbon/react";
import type { PromptRequest } from "@/App";
import { t } from "@/lib/i18n";
import { submitOnEnter } from "@/lib/submitOnEnter";
import { useReturnFocus } from "@/state/useReturnFocus";

interface PromptModalProps {
  request: PromptRequest | null;
  onClose: () => void;
}

/** Single-field text prompt (add mirror, per-job bandwidth cap). */
export function PromptModal({ request, onClose }: PromptModalProps) {
  const [value, setValue] = useState("");
  const [touched, setTouched] = useState(false);
  useReturnFocus(request != null);

  useEffect(() => {
    setValue("");
    setTouched(false);
  }, [request]);

  if (!request) return null;
  const error = request.validate(value);
  const invalid = touched && error != null;
  const submit = () => {
    setTouched(true);
    if (error != null) return;
    const onSubmit = request.onSubmit;
    onClose();
    void onSubmit(value);
  };

  return (
    <Modal
      open
      size="sm"
      modalHeading={request.title}
      primaryButtonText={request.confirmLabel}
      secondaryButtonText={t("discard.cancel")}
      primaryButtonDisabled={error != null}
      onRequestClose={onClose}
      onRequestSubmit={submit}
    >
      <TextInput
        id="prompt-input"
        data-modal-primary-focus
        labelText={request.label}
        placeholder={request.placeholder}
        value={value}
        invalid={invalid}
        invalidText={error ?? ""}
        onKeyDown={(e) => submitOnEnter(e, submit)}
        onChange={(e) => {
          setValue(e.target.value);
          setTouched(true);
        }}
      />
    </Modal>
  );
}
