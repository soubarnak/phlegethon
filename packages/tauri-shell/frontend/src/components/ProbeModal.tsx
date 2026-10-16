import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Accordion,
  AccordionItem,
  Button,
  Checkbox,
  ComposedModal,
  Dropdown,
  DropdownSkeleton,
  InlineLoading,
  InlineNotification,
  ModalBody,
  ModalFooter,
  ModalHeader,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  Tag,
} from "@carbon/react";
import { api } from "@/tauri/api";
import type {
  ExtractionTier,
  MediaExtrasCapabilities,
  MediaExtrasOptions,
  MediaItem,
  ProbeResult,
} from "@/tauri/types";
import { EMPTY_MEDIA_EXTRAS } from "@/tauri/types";
import { errorHint, parseCoreError, type ParsedError } from "@/lib/errors";
import { t } from "@/lib/i18n";
import { formatBytes } from "@/lib/progress";
import {
  applyQualityToAll,
  buildSelection,
  chosenQuality,
  commonQualityOptions,
  defaultSelectedIds,
  firstQualityId,
  invertSelection,
  remapQualityAfterEnrich,
  totalSelectedBytes,
  type QualityByItem,
} from "@/lib/probeSelection";
import { useReturnFocus } from "@/state/useReturnFocus";
import { FOLDER_DIALOG_CLASS, FolderPicker } from "./FolderPicker";

interface ProbeModalProps {
  /** URL to probe; `null` closes the modal. */
  url: string | null;
  /** Initial Save-to override from the URL bar. */
  initialDestination: string | null;
  onClose: () => void;
  /** Jobs were created. */
  onConfirmed: (jobIds: string[]) => void;
  /** "Try plain download" after a failed probe. */
  onTryPlain: (url: string) => void;
  /** Jump to Settings (cookie hint). */
  onOpenSettings: () => void;
  onError: (title: string, err: unknown) => void;
}

type Phase =
  | { kind: "probing" }
  | { kind: "error"; error: ParsedError }
  | { kind: "results"; result: ProbeResult };

const MEDIA_EXTRA_FIELDS: Array<{ key: keyof MediaExtrasOptions; label: string; needsFfmpeg: boolean }> = [
  { key: "write_subs", label: "probe.media.writeSubs", needsFfmpeg: false },
  { key: "embed_subs", label: "probe.media.embedSubs", needsFfmpeg: true },
  { key: "write_thumbnail", label: "probe.media.writeThumbnail", needsFfmpeg: false },
  { key: "embed_thumbnail", label: "probe.media.embedThumbnail", needsFfmpeg: true },
  { key: "write_metadata", label: "probe.media.writeMetadata", needsFfmpeg: false },
  { key: "embed_metadata", label: "probe.media.embedMetadata", needsFfmpeg: true },
  { key: "write_chapters", label: "probe.media.writeChapters", needsFfmpeg: false },
  { key: "embed_chapters", label: "probe.media.embedChapters", needsFfmpeg: true },
  { key: "remux", label: "probe.media.remux", needsFfmpeg: true },
];

function tierNotice(tier: ExtractionTier): { title: string; subtitle: string } | null {
  switch (tier) {
    case "generic_extractor":
      return { title: t("probe.generic.title"), subtitle: t("probe.generic.body") };
    case "page_sniff":
      return { title: t("probe.sniff.title"), subtitle: t("probe.sniff.body") };
    case "direct_file":
      return { title: t("probe.file.title"), subtitle: t("probe.file.body") };
    default:
      return null;
  }
}

export function ProbeModal({
  url,
  initialDestination,
  onClose,
  onConfirmed,
  onTryPlain,
  onOpenSettings,
  onError,
}: ProbeModalProps) {
  const [phase, setPhase] = useState<Phase>({ kind: "probing" });
  const [items, setItems] = useState<MediaItem[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [qualityByItem, setQualityByItem] = useState<QualityByItem>({});
  const [enriching, setEnriching] = useState<Set<string>>(new Set());
  const [destination, setDestination] = useState<string | null>(initialDestination);
  const [destPreview, setDestPreview] = useState<string | null>(null);
  const [extras, setExtras] = useState<MediaExtrasOptions>(EMPTY_MEDIA_EXTRAS);
  const [caps, setCaps] = useState<MediaExtrasCapabilities>({ ffmpeg_available: true, embed_disabled_reason: null });
  const [confirming, setConfirming] = useState(false);
  const enrichedRef = useRef(new Set<string>());
  const itemsRef = useRef<MediaItem[]>([]);
  itemsRef.current = items;
  const previewSeq = useRef(0);
  // The in-flight `probe_url` call, so closing can wait for it to settle (M-62).
  const probeInFlight = useRef<Promise<unknown> | null>(null);
  useReturnFocus(url != null);

  // ---- run the probe when a URL is given
  useEffect(() => {
    if (!url) return;
    let cancelled = false;
    setPhase({ kind: "probing" });
    setItems([]);
    setSelected(new Set());
    setQualityByItem({});
    setDestination(initialDestination);
    setDestPreview(null);
    enrichedRef.current = new Set();

    (async () => {
      const [defaults, capabilities] = await Promise.all([
        api.getDefaultMediaExtras().catch(() => EMPTY_MEDIA_EXTRAS),
        api.getMediaExtrasCapabilities().catch(() => ({ ffmpeg_available: false, embed_disabled_reason: null })),
      ]);
      if (cancelled) return;
      setExtras(defaults);
      setCaps(capabilities);
      try {
        const probing = api.probeUrl(url);
        probeInFlight.current = probing;
        const result = await probing;
        if (cancelled) return;
        // Pre-pick a quality per item using the Default Quality Policy.
        const picks = await Promise.all(
          result.items.map(async (item) => {
            const id = await api.selectDefaultQuality(item.qualities).catch(() => null);
            return [item.id, id ?? firstQualityId(item)] as const;
          }),
        );
        if (cancelled) return;
        setItems(result.items);
        setSelected(defaultSelectedIds(result.items));
        setQualityByItem(Object.fromEntries(picks.filter(([, id]) => id != null) as Array<[string, string]>));
        setPhase({ kind: "results", result });
      } catch (err) {
        if (cancelled) return;
        setPhase({ kind: "error", error: parseCoreError(err) });
      }
    })();

    return () => {
      cancelled = true;
    };
    // initialDestination is only the seed; later edits happen inside the modal.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [url]);

  const result = phase.kind === "results" ? phase.result : null;
  const selectionCount = selected.size;
  const totals = useMemo(() => totalSelectedBytes(items, selected, qualityByItem), [items, selected, qualityByItem]);
  const qualityOptions = useMemo(() => commonQualityOptions(items), [items]);

  // ---- destination preview (debounced; stale replies are dropped)
  useEffect(() => {
    if (!url || !result || selectionCount === 0) {
      setDestPreview(null);
      return;
    }
    const seq = ++previewSeq.current;
    const timer = window.setTimeout(() => {
      const selection = buildSelection(url, items, selected, qualityByItem, null);
      api
        .previewProbeDestination(selection, destination)
        .then((preview) => {
          if (seq === previewSeq.current) setDestPreview(preview.path_display);
        })
        .catch(() => {
          if (seq === previewSeq.current) setDestPreview(null);
        });
    }, 150);
    return () => window.clearTimeout(timer);
  }, [url, result, items, selected, qualityByItem, destination, selectionCount]);

  const close = useCallback(() => {
    // Closing mid-confirm would drop the sheet while its jobs are still being created (M-63).
    if (confirming) return;
    if (url) {
      const clear = () => void api.clearPendingProbe(url).catch(() => undefined);
      const inFlight = phase.kind === "probing" ? probeInFlight.current : null;
      if (phase.kind === "probing") void api.cancelProbe(url).catch(() => undefined);
      // Core stores a probe result when the probe settles, even a cancelled one. Clearing
      // before that leaves a pending probe that blocks the URL until restart (M-62).
      if (inFlight) void inFlight.then(clear, clear);
      else clear();
    }
    onClose();
  }, [url, phase.kind, confirming, onClose]);

  const toggleItem = (id: string, on: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  };

  // Flat playlist entries start with preset qualities; fetch the exact formats lazily.
  const ensureEnriched = useCallback(
    async (itemId: string) => {
      const item = itemsRef.current.find((i) => i.id === itemId);
      if (!item || !url || !item.qualities_are_presets || enrichedRef.current.has(itemId)) return;
      enrichedRef.current.add(itemId);
      setEnriching((prev) => new Set(prev).add(itemId));
      try {
        const qualities = await api.enrichProbeItemQualities(url, itemId);
        setItems((prev) =>
          prev.map((i) => (i.id === itemId ? { ...i, qualities, qualities_are_presets: false } : i)),
        );
        setQualityByItem((prev) => {
          const picked = remapQualityAfterEnrich(prev[itemId], qualities);
          return picked ? { ...prev, [itemId]: picked } : prev;
        });
      } catch (err) {
        enrichedRef.current.delete(itemId);
        onError(t("probe.enrichFailed"), err);
      } finally {
        setEnriching((prev) => {
          const next = new Set(prev);
          next.delete(itemId);
          return next;
        });
      }
    },
    [url, onError],
  );

  const confirm = async () => {
    if (!url || selectionCount === 0) return;
    setConfirming(true);
    try {
      const selection = buildSelection(url, items, selected, qualityByItem, extras);
      if (selection.items.length === 0) return;
      const jobIds = await api.confirmProbeSelection(selection, destination);
      onConfirmed(jobIds);
    } catch (err) {
      onError(t("probe.confirmFailed"), err);
    } finally {
      setConfirming(false);
    }
  };

  const notice = result ? tierNotice(result.tier) : null;
  const heading =
    phase.kind === "probing" ? t("probe.probingTitle") : phase.kind === "error" ? t("probe.failedTitle") : result?.source_name ?? t("probe.results");

  if (url == null) return null;

  return (
    <ComposedModal
      open
      size="lg"
      className="ph-probe"
      onClose={close}
      preventCloseOnClickOutside
      selectorsFloatingMenus={[`.${FOLDER_DIALOG_CLASS}`]}
    >
      <ModalHeader title={heading} label={t("probe.title")} closeModal={close} />
      <ModalBody hasScrollingContent={phase.kind === "results"}>
        {phase.kind === "probing" && (
          <div className="ph-probe__loading" data-testid="probe-loading">
            <InlineLoading status="active" description={t("probe.probing")} />
            <span className="ph-probe__url">{url}</span>
          </div>
        )}

        {phase.kind === "error" && url && (
          <div data-testid="probe-error">
            <InlineNotification
              kind="error"
              lowContrast
              hideCloseButton
              title={t("probe.failedTitle")}
              subtitle={phase.error.message}
            />
            {errorHint(phase.error.code) && <p className="ph-muted" style={{ marginTop: "0.75rem" }}>{errorHint(phase.error.code)}</p>}
            <p className="ph-probe__url" style={{ marginTop: "0.75rem" }}>{url}</p>
            <div style={{ display: "flex", gap: "0.5rem", marginTop: "1rem", flexWrap: "wrap" }}>
              <Button kind="tertiary" size="md" onClick={() => onTryPlain(url)}>
                {t("probe.tryPlain")}
              </Button>
              {phase.error.code === "needs_browser_cookies" && (
                <Button kind="ghost" size="md" onClick={onOpenSettings}>
                  {t("probe.openCookieSettings")}
                </Button>
              )}
            </div>
          </div>
        )}

        {result && (
          <>
            {notice && (
              <InlineNotification
                kind="warning"
                lowContrast
                hideCloseButton
                title={notice.title}
                subtitle={notice.subtitle}
                data-testid="probe-tier-notice"
              />
            )}
            {result.warnings.map((w) => (
              <p key={w} className="ph-muted" style={{ margin: "0.5rem 0" }}>
                {w}
              </p>
            ))}
            <div className="ph-probe__summary">
              <span>
                {selectionCount === 1 ? t("probe.selection.one") : t("probe.selection.many", { count: selectionCount })}
              </span>
              <span>
                {t("probe.of", { total: items.length })}
              </span>
              {totals.bytes > 0 && (
                <span data-testid="probe-total-size">
                  {t("probe.totalSize", { size: formatBytes(totals.bytes) })}
                  {totals.unknown > 0 ? ` ${t("probe.plusUnknown", { count: totals.unknown })}` : ""}
                </span>
              )}
            </div>
            <div className="ph-probe__toolbar">
              <Button kind="ghost" size="sm" onClick={() => setSelected(new Set(items.map((i) => i.id)))}>
                {t("probe.selectAll")}
              </Button>
              <Button kind="ghost" size="sm" onClick={() => setSelected(new Set())}>
                {t("probe.deselectAll")}
              </Button>
              <Button kind="ghost" size="sm" onClick={() => setSelected(invertSelection(items, selected))}>
                {t("probe.invert")}
              </Button>
              <span className="ph-probe__spacer" />
              {qualityOptions.length > 1 && (
                <Dropdown
                  id="probe-apply-quality"
                  autoAlign
                  size="sm"
                  titleText={t("probe.applyQualityAll")}
                  hideLabel
                  label={t("probe.applyQualityAll")}
                  items={qualityOptions}
                  itemToString={(q) => q?.label ?? ""}
                  selectedItem={null}
                  onChange={({ selectedItem }) => {
                    if (selectedItem) {
                      setQualityByItem((prev) => applyQualityToAll(items, selected, selectedItem.id, prev));
                    }
                  }}
                />
              )}
            </div>
            <Table size="md" aria-label={t("probe.results")}>
              <TableHead>
                <TableRow>
                  <TableHeader className="ph-probe__col ph-probe__col--select" style={{ width: "3rem" }}>
                    <Checkbox
                      id="probe-select-all"
                      labelText={t("probe.selectAll")}
                      hideLabel
                      checked={selectionCount === items.length && items.length > 0}
                      indeterminate={selectionCount > 0 && selectionCount < items.length}
                      onChange={(_, { checked }) => setSelected(checked ? new Set(items.map((i) => i.id)) : new Set())}
                    />
                  </TableHeader>
                  <TableHeader className="ph-probe__col ph-probe__col--title">{t("queue.col.title")}</TableHeader>
                  <TableHeader className="ph-probe__col ph-probe__col--quality">{t("probe.quality")}</TableHeader>
                  <TableHeader className="ph-probe__col ph-probe__col--size">{t("queue.col.size")}</TableHeader>
                </TableRow>
              </TableHead>
              <TableBody>
                {items.map((item) => {
                  const q = chosenQuality(item, qualityByItem);
                  const busy = enriching.has(item.id);
                  const meta = [item.uploader, item.upload_date ? formatUploadDate(item.upload_date) : null].filter(Boolean).join(" · ");
                  return (
                    <TableRow key={item.id} data-testid={`probe-row-${item.id}`}>
                      <TableCell className="ph-probe__col ph-probe__col--select">
                        <Checkbox
                          id={`probe-pick-${item.id}`}
                          labelText={t("probe.selectItem", { title: item.title })}
                          hideLabel
                          checked={selected.has(item.id)}
                          onChange={(_, { checked }) => toggleItem(item.id, checked)}
                        />
                      </TableCell>
                      <TableCell className="ph-probe__col ph-probe__col--title">
                        <div className="ph-probe__item-title" title={item.title}>
                          {item.title}
                        </div>
                        <div className="ph-probe__item-sub">
                          {item.archived && (
                            <Tag size="sm" type="green" data-testid="archived-tag">
                              {t("probe.downloaded")}
                            </Tag>
                          )}{" "}
                          {meta}
                        </div>
                      </TableCell>
                      <TableCell className="ph-probe__col ph-probe__col--quality">
                        <div
                          className="ph-probe__quality"
                          onFocusCapture={() => void ensureEnriched(item.id)}
                          onMouseDownCapture={() => void ensureEnriched(item.id)}
                        >
                          {busy ? (
                            <DropdownSkeleton size="sm" hideLabel />
                          ) : (
                            <Dropdown
                              id={`probe-quality-${item.id}`}
                              autoAlign
                              size="sm"
                              hideLabel
                              titleText={t("probe.qualityFor", { title: item.title })}
                              label={t("probe.quality")}
                              items={item.qualities}
                              itemToString={(x) => (x ? x.label : "")}
                              selectedItem={q}
                              onChange={({ selectedItem }) => {
                                if (selectedItem) {
                                  setQualityByItem((prev) => ({ ...prev, [item.id]: selectedItem.id }));
                                  toggleItem(item.id, true);
                                }
                              }}
                            />
                          )}
                        </div>
                      </TableCell>
                      <TableCell className="ph-probe__col ph-probe__col--size">{q?.size_bytes ? formatBytes(q.size_bytes) : "—"}</TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>

            <div className="ph-probe__section">
              <Accordion size="md">
                <AccordionItem title={t("probe.mediaOptions")}>
                  {!caps.ffmpeg_available && (
                    <p className="ph-muted" style={{ marginBottom: "0.75rem" }}>
                      {t("probe.media.noFfmpeg")}
                    </p>
                  )}
                  <div className="ph-settings__grid">
                    {MEDIA_EXTRA_FIELDS.map((f) => (
                      <Checkbox
                        key={f.key}
                        id={`probe-extra-${f.key}`}
                        labelText={t(f.label)}
                        checked={extras[f.key] && !(f.needsFfmpeg && !caps.ffmpeg_available)}
                        disabled={f.needsFfmpeg && !caps.ffmpeg_available}
                        onChange={(_, { checked }) => setExtras((prev) => ({ ...prev, [f.key]: checked }))}
                      />
                    ))}
                  </div>
                </AccordionItem>
              </Accordion>
            </div>

            <div className="ph-probe__dest">
              <FolderPicker
                label={t("probe.saveTo")}
                size="sm"
                onPicked={(p) => setDestination(p)}
                onError={() => undefined}
              />
              {destination && (
                <Button kind="ghost" size="sm" onClick={() => setDestination(null)}>
                  {t("probe.libraryRootDefault")}
                </Button>
              )}
              <span className="ph-probe__dest-path" data-testid="probe-dest-preview">
                {destPreview ?? (destination ? destination : t("probe.libraryRootDefault"))}
                {selectionCount > 1 ? ` ${t("probe.eachSubfolder")}` : ""}
              </span>
            </div>
          </>
        )}
      </ModalBody>
      <ModalFooter>
        <Button kind="secondary" disabled={confirming} onClick={close}>
          {t("probe.cancel")}
        </Button>
        {result && (
          <Button
            kind="primary"
            disabled={selectionCount === 0 || confirming}
            onClick={() => void confirm()}
            data-testid="probe-confirm"
          >
            {selectionCount === 0
              ? t("probe.confirm")
              : t("probe.confirmN", { count: selectionCount })}
          </Button>
        )}
      </ModalFooter>
    </ComposedModal>
  );
}

/** `20260101` → `2026-01-01`. */
function formatUploadDate(raw: string): string {
  return /^\d{8}$/.test(raw) ? `${raw.slice(0, 4)}-${raw.slice(4, 6)}-${raw.slice(6, 8)}` : raw;
}
