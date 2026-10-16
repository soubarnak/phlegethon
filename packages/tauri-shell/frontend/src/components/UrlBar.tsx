import { useCallback, useEffect, useRef, useState, type DragEvent, type KeyboardEvent } from "react";
import { ActionableNotification, Button, Tag, TextArea, Toggle } from "@carbon/react";
import { Add } from "@carbon/icons-react";
import { t } from "@/lib/i18n";
import { textFromDataTransfer } from "@/lib/urlInput";
import { createClipboardWatcher } from "@/lib/clipboardWatcher";
import { isOwnClipboardWrite, readClipboardText } from "@/tauri/platform";
import { api } from "@/tauri/api";
import { FolderPicker } from "./FolderPicker";

interface UrlBarProps {
  /** Add the given text (one or many URLs). Resolves when all were handled. */
  onSubmit: (text: string) => Promise<void>;
  destination: string | null;
  onDestinationChange: (path: string | null) => void;
  clipboardWatch: boolean;
  onClipboardWatchChange: (enabled: boolean) => void;
  busy?: boolean;
}

export function UrlBar({
  onSubmit,
  destination,
  onDestinationChange,
  clipboardWatch,
  onClipboardWatchChange,
  busy = false,
}: UrlBarProps) {
  const [text, setText] = useState("");
  const [dragging, setDragging] = useState(false);
  const [clipboardHit, setClipboardHit] = useState<string[] | null>(null);
  const textRef = useRef<HTMLTextAreaElement | null>(null);

  const submit = useCallback(async () => {
    const value = text.trim();
    if (!value || busy) return;
    await onSubmit(value);
    setText("");
    textRef.current?.focus();
  }, [text, busy, onSubmit]);

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    // Enter that confirms an IME composition must not submit (L-56).
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing && e.keyCode !== 229) {
      e.preventDefault();
      void submit();
    }
  };

  const onDrop = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setDragging(false);
    const dropped = textFromDataTransfer(e.dataTransfer);
    if (dropped) {
      void onSubmit(dropped);
    }
  };

  // Ctrl+V anywhere outside an input adds the clipboard contents.
  useEffect(() => {
    const handler = (e: ClipboardEvent) => {
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName?.toLowerCase();
      if (tag === "input" || tag === "textarea" || target?.isContentEditable) return;
      const pasted = e.clipboardData?.getData("text/plain")?.trim();
      if (pasted) {
        e.preventDefault();
        void onSubmit(pasted);
      }
    };
    document.addEventListener("paste", handler);
    return () => document.removeEventListener("paste", handler);
  }, [onSubmit]);

  // Opt-in clipboard watcher: offers URLs, never auto-enqueues.
  useEffect(() => {
    if (!clipboardWatch) {
      setClipboardHit(null);
      return;
    }
    const watcher = createClipboardWatcher({
      readClipboardText,
      splitUrls: (value) => api.splitUrls(value),
      onUrlsDetected: (urls) => setClipboardHit(urls),
      isOwnWrite: isOwnClipboardWrite,
    });
    watcher.start();
    // Polling pauses while the window is unfocused; look right away when it returns.
    const onFocus = () => void watcher.tick();
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("focus", onFocus);
      watcher.stop();
    };
  }, [clipboardWatch]);

  return (
    <section
      className={`ph-urlbar${dragging ? " is-dragging" : ""}`}
      aria-label={t("url.sectionLabel")}
      onDragOver={(e) => {
        e.preventDefault();
        if (!dragging) setDragging(true);
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={onDrop}
    >
      <TextArea
        id="url-input"
        ref={textRef}
        labelText={t("url.label")}
        placeholder={t("url.placeholder")}
        rows={1}
        value={text}
        disabled={busy}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKeyDown}
        helperText={t("url.helper")}
      />
      <div className="ph-urlbar__actions">
        <Button kind="primary" renderIcon={Add} disabled={busy || !text.trim()} onClick={() => void submit()}>
          {t("url.add")}
        </Button>
      </div>
      <div className="ph-urlbar__meta">
        <FolderPicker size="sm" onPicked={(p) => onDestinationChange(p)} />
        {destination ? (
          <Tag
            className="ph-dest-tag"
            type="blue"
            size="md"
            filter
            title={t("url.clearDestination")}
            onClose={() => onDestinationChange(null)}
          >
            {t("url.saveToTag", { path: destination })}
          </Tag>
        ) : (
          <span className="ph-muted">{t("probe.libraryRootDefault")}</span>
        )}
        <Toggle
          id="clipboard-watch"
          size="sm"
          labelText={t("url.clipboardWatch")}
          labelA={t("common.off")}
          labelB={t("common.on")}
          toggled={clipboardWatch}
          onToggle={(checked) => onClipboardWatchChange(checked)}
        />
        {clipboardHit && clipboardHit.length > 0 && (
          <ActionableNotification
            kind="info"
            lowContrast
            inline
            title={t("url.clipboardDetected", { count: clipboardHit.length })}
            subtitle={clipboardHit[0]}
            actionButtonLabel={t("url.add")}
            onActionButtonClick={() => {
              const urls = clipboardHit.join("\n");
              setClipboardHit(null);
              void onSubmit(urls);
            }}
            onClose={() => {
              setClipboardHit(null);
              return false;
            }}
          />
        )}
      </div>
    </section>
  );
}
