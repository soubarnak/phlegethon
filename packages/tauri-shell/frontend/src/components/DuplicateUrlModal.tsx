import { Button, ComposedModal, ModalBody, ModalFooter, ModalHeader, Tag } from "@carbon/react";
import type { ActiveJobMatch } from "@/tauri/types";
import type { DuplicateResolution } from "@/lib/urlInput";
import { t } from "@/lib/i18n";

interface DuplicateUrlModalProps {
  match: ActiveJobMatch | null;
  onResolve: (resolution: DuplicateResolution) => void;
}

export function DuplicateUrlModal({ match, onResolve }: DuplicateUrlModalProps) {
  if (!match) return null;
  return (
    <ComposedModal open size="sm" onClose={() => onResolve("cancel")}>
      <ModalHeader title={t("duplicate.title")} label={t("duplicate.label")} />
      <ModalBody>
        <p>{t("duplicate.body")}</p>
        <p style={{ marginTop: "0.75rem" }}>
          <strong>{match.title}</strong> <Tag size="sm" type="gray">{match.status}</Tag>
          <br />
          <span className="ph-mono ph-break">{match.url}</span>
        </p>
      </ModalBody>
      <ModalFooter>
        <Button kind="secondary" onClick={() => onResolve("cancel")}>
          {t("discard.cancel")}
        </Button>
        <Button kind="tertiary" onClick={() => onResolve("focus")}>
          {t("duplicate.focus")}
        </Button>
        <Button kind="primary" onClick={() => onResolve("enqueue")}>
          {t("duplicate.enqueue")}
        </Button>
      </ModalFooter>
    </ComposedModal>
  );
}
