import { useEffect, useState } from "react";
import { Button, Modal, Select, SelectItem, TextInput } from "@carbon/react";
import { api } from "@/tauri/api";
import { t } from "@/lib/i18n";
import {
  EXPECTED_HASH_ALGORITHMS,
  expectedDigestLength,
  normalizeExpectedDigest,
  type ExpectedDigestResult,
  type ExpectedHashAlgorithm,
} from "@/lib/expectedHash";
import { submitOnEnter } from "@/lib/submitOnEnter";
import type { ExpectedHash } from "@/tauri/types";
import { useReturnFocus } from "@/state/useReturnFocus";
import { useToasts } from "@/state/useToasts";

export interface ExpectedHashRequest {
  jobId: string;
  title: string;
}

interface ExpectedHashModalProps {
  /** The job to set the hash on, or null when the dialog is closed. */
  request: ExpectedHashRequest | null;
  onClose: () => void;
}

function digestErrorText(result: ExpectedDigestResult): string | null {
  if (result.ok) return null;
  switch (result.reason) {
    case "empty":
      return t("hash.err.empty");
    case "not_hex":
      return t("hash.err.notHex");
    case "wrong_length":
      return t("hash.err.length", { expected: result.expectedLength, actual: result.actualLength });
  }
}

/**
 * Set Expected hash for Integrity Check. The digest is checked here first; the Core
 * checks it again when it is saved.
 */
export function ExpectedHashModal({ request, onClose }: ExpectedHashModalProps) {
  const toasts = useToasts();
  const [algorithm, setAlgorithm] = useState<ExpectedHashAlgorithm>("sha-256");
  const [digest, setDigest] = useState("");
  const [touched, setTouched] = useState(false);
  const [saving, setSaving] = useState(false);
  const [current, setCurrent] = useState<ExpectedHash | null>(null);
  useReturnFocus(request != null);

  useEffect(() => {
    setAlgorithm("sha-256");
    setDigest("");
    setTouched(false);
    setCurrent(null);
    if (!request) return;
    let cancelled = false;
    api
      .getJobExpectedHash(request.jobId)
      .then((hash) => {
        if (!cancelled) setCurrent(hash);
      })
      .catch(() => {
        // The dialog still works for setting a hash; it just cannot show the current one.
      });
    return () => {
      cancelled = true;
    };
  }, [request]);

  if (!request) return null;
  const result = normalizeExpectedDigest(algorithm, digest);
  const error = digestErrorText(result);
  const invalid = touched && error != null;

  const submit = async () => {
    setTouched(true);
    if (!result.ok || saving) return;
    setSaving(true);
    try {
      await api.setJobExpectedHash(request.jobId, algorithm, result.digest);
      toasts.success(t("hash.saved"));
      onClose();
    } catch (err) {
      toasts.error(t("hash.failed"), err);
    } finally {
      setSaving(false);
    }
  };

  const clear = async () => {
    if (saving) return;
    setSaving(true);
    try {
      await api.clearJobExpectedHash(request.jobId);
      toasts.success(t("hash.cleared"));
      onClose();
    } catch (err) {
      toasts.error(t("hash.clearFailed"), err);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      open
      size="sm"
      modalHeading={t("hash.title")}
      primaryButtonText={t("hash.save")}
      secondaryButtonText={t("discard.cancel")}
      primaryButtonDisabled={error != null || saving}
      onRequestClose={onClose}
      onRequestSubmit={() => void submit()}
    >
      <p style={{ wordBreak: "break-word" }}>{t("hash.body", { title: request.title })}</p>
      {current && (
        <div style={{ marginBottom: "1rem" }}>
          <p>{t("hash.current", { algorithm: t(`hash.algorithm.${current.algorithm}`) })}</p>
          <code data-testid="expected-hash-current" style={{ wordBreak: "break-all" }}>
            {current.digest}
          </code>
          <div style={{ marginTop: "0.5rem" }}>
            <Button kind="danger--tertiary" size="sm" disabled={saving} onClick={() => void clear()}>
              {t("hash.clear")}
            </Button>
          </div>
        </div>
      )}
      <Select
        id="expected-hash-algorithm"
        labelText={t("hash.algorithm")}
        value={algorithm}
        onChange={(e) => setAlgorithm(e.target.value as ExpectedHashAlgorithm)}
      >
        {EXPECTED_HASH_ALGORITHMS.map((name) => (
          <SelectItem key={name} value={name} text={t(`hash.algorithm.${name}`)} />
        ))}
      </Select>
      <TextInput
        id="expected-hash-digest"
        data-modal-primary-focus
        labelText={t("hash.digest")}
        placeholder={t("hash.digestPlaceholder")}
        helperText={t("hash.expectedLength", { length: expectedDigestLength(algorithm) })}
        value={digest}
        invalid={invalid}
        invalidText={error ?? ""}
        autoComplete="off"
        spellCheck={false}
        onKeyDown={(e) => submitOnEnter(e, () => void submit())}
        onChange={(e) => {
          setDigest(e.target.value);
          setTouched(true);
        }}
      />
    </Modal>
  );
}
