import { useEffect, useState } from "react";
import { InlineLoading, Modal, Tag } from "@carbon/react";
import { api } from "@/tauri/api";
import { t } from "@/lib/i18n";
import { useReturnFocus } from "@/state/useReturnFocus";
import { useToasts } from "@/state/useToasts";

export interface MirrorsRequest {
  jobId: string;
  title: string;
}

interface MirrorsModalProps {
  /** The job whose Mirror Set to show, or null when the dialog is closed. */
  request: MirrorsRequest | null;
  /** The URL the job is downloading from now. Live, so a failover moves the marker. */
  currentUrl: string | null;
  onClose: () => void;
}

/** Read-only Mirror Set of one job, in the order the Core tries them. */
export function MirrorsModal({ request, currentUrl, onClose }: MirrorsModalProps) {
  const toasts = useToasts();
  const [urls, setUrls] = useState<string[] | null>(null);
  useReturnFocus(request != null);

  useEffect(() => {
    if (!request) return;
    let live = true;
    setUrls(null);
    api.jobMirrors(request.jobId).then(
      (list) => {
        if (live) setUrls(list);
      },
      (err) => {
        if (!live) return;
        toasts.error(t("mirrors.failed"), err);
        onClose();
      },
    );
    return () => {
      live = false;
    };
    // Fetch once per open. onClose is a new closure on every App render, so it is left out.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [request]);

  if (!request) return null;

  return (
    <Modal open passiveModal size="sm" modalHeading={t("mirrors.title")} onRequestClose={onClose}>
      <p style={{ wordBreak: "break-word" }}>{request.title}</p>
      <p className="ph-muted">{t("mirrors.body")}</p>
      {urls == null ? (
        <InlineLoading description={t("mirrors.loading")} />
      ) : urls.length === 0 ? (
        <p>{t("mirrors.none")}</p>
      ) : (
        <ol className="ph-mirrors" data-testid="mirror-list">
          {urls.map((url, index) => {
            const current = url === currentUrl;
            return (
              <li key={`${index}-${url}`} aria-current={current ? "true" : undefined}>
                <span className="ph-mono ph-break">{url}</span>
                {current && <Tag size="sm" type="blue">{t("mirrors.current")}</Tag>}
              </li>
            );
          })}
        </ol>
      )}
    </Modal>
  );
}
