import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Button, Checkbox, Dropdown, InlineNotification, NumberInput, TextInput, Toggle } from "@carbon/react";
import { api } from "@/tauri/api";
import type {
  CookieChannel,
  DefaultQualityPolicy,
  MediaExtrasCapabilities,
  MediaExtrasOptions,
  ProbeTierSettings,
  ToolchainStatus,
} from "@/tauri/types";
import { EMPTY_MEDIA_EXTRAS } from "@/tauri/types";
import { t } from "@/lib/i18n";
import { useToasts } from "@/state/useToasts";
import { FolderPicker } from "./FolderPicker";
import { ToolchainPanel } from "./ToolchainPanel";

interface SettingsPageProps {
  notificationsEnabled: boolean;
  onNotificationsChange: (enabled: boolean) => void;
  onToolchainStatus: (status: ToolchainStatus | null) => void;
}

const HEIGHTS = [4320, 2160, 1440, 1080, 720, 480, 360].map((h) => ({ id: h, label: `${h}p` }));

// A function, not a constant: t() at module scope runs before initLocale() (L-59).
const browserOptions = () => [
  { id: "", label: t("settings.cookies.none") },
  { id: "firefox", label: "Firefox" },
  { id: "chrome", label: "Chrome" },
  { id: "chromium", label: "Chromium" },
  { id: "brave", label: "Brave" },
  { id: "vivaldi", label: "Vivaldi" },
  { id: "opera", label: "Opera" },
  { id: "edge", label: "Edge" },
];

const MEDIA_FIELDS: Array<{ key: keyof MediaExtrasOptions; label: string; needsFfmpeg: boolean }> = [
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

/** Each setting loads on its own; one that fails keeps its Apply disabled so defaults are never written back (M-60). */
type SettingKey =
  | "libraryRoot"
  | "activeLimit"
  | "audioOnly"
  | "quality"
  | "template"
  | "extras"
  | "cookies"
  | "proxy"
  | "bandwidth"
  | "tiers";

/** Order of the `Promise.allSettled` calls in the loader; null marks a call that blocks nothing. */
const LOAD_ORDER: Array<SettingKey | null> = [
  "libraryRoot",
  "activeLimit",
  "audioOnly",
  "quality",
  "template",
  "extras",
  null, // media extras capabilities
  "cookies",
  "proxy",
  "bandwidth",
  "tiers",
];

function Section({ id, title, hint, children }: { id?: string; title: string; hint?: string; children: ReactNode }) {
  return (
    <section className="ph-settings__section" id={id}>
      <h3>{title}</h3>
      {hint && <p className="ph-settings__hint">{hint}</p>}
      {children}
    </section>
  );
}

export function SettingsPage({ notificationsEnabled, onNotificationsChange, onToolchainStatus }: SettingsPageProps) {
  const toasts = useToasts();
  const browsers = useMemo(browserOptions, []);

  const [libraryRoot, setLibraryRoot] = useState("");
  const [activeLimit, setActiveLimit] = useState(3);
  const [audioOnly, setAudioOnly] = useState(false);
  const [policy, setPolicy] = useState<DefaultQualityPolicy>({ max_height: 1080, prefer_mp4: true });
  const [template, setTemplate] = useState("");
  const [templatePreview, setTemplatePreview] = useState("");
  const [templateError, setTemplateError] = useState<string | null>(null);
  const [extras, setExtras] = useState<MediaExtrasOptions>(EMPTY_MEDIA_EXTRAS);
  const [caps, setCaps] = useState<MediaExtrasCapabilities>({ ffmpeg_available: true, embed_disabled_reason: null });
  const [cookies, setCookies] = useState<CookieChannel>({ cookies_from_browser: null, cookie_file: null });
  const [proxy, setProxy] = useState("");
  const [capKb, setCapKb] = useState(0);
  const [tiers, setTiers] = useState<ProbeTierSettings>({
    allow_generic: true,
    allow_page_sniff: true,
    allow_direct_fallback: true,
    probe_timeout_secs: 45,
  });

  const [failed, setFailed] = useState<ReadonlySet<SettingKey>>(new Set());
  const isBlocked = (...keys: SettingKey[]) => keys.some((k) => failed.has(k));
  const loadSeq = useRef(0);

  // ---- load every setting independently; only the ones that loaded are applied to state
  const loadAll = useCallback(() => {
    const seq = ++loadSeq.current;
    (async () => {
      const results = await Promise.allSettled([
        api.getLibraryRoot(),
        api.getActiveJobLimit(),
        api.getAudioOnlyMode(),
        api.getDefaultQualityPolicy(),
        api.getFilenameTemplate(),
        api.getDefaultMediaExtras(),
        api.getMediaExtrasCapabilities(),
        api.getCookieChannel(),
        api.getProxySettings(),
        api.getGlobalBandwidthCap(),
        api.getProbeTierSettings(),
      ]);
      if (seq !== loadSeq.current) return;
      const bad = new Set<SettingKey>();
      results.forEach((result, i) => {
        const key = LOAD_ORDER[i];
        if (result.status === "rejected" && key) bad.add(key);
      });
      setFailed(bad);
      const ok = <T,>(i: number): { value: T } | null =>
        results[i].status === "fulfilled" ? { value: (results[i] as PromiseFulfilledResult<T>).value } : null;
      const set = <T,>(i: number, apply: (value: T) => void) => {
        const loaded = ok<T>(i);
        if (loaded) apply(loaded.value);
      };
      set<string | null>(0, (v) => setLibraryRoot(v ?? ""));
      set<number>(1, setActiveLimit);
      set<boolean>(2, setAudioOnly);
      set<DefaultQualityPolicy>(3, setPolicy);
      set<string>(4, setTemplate);
      set<MediaExtrasOptions>(5, setExtras);
      set<MediaExtrasCapabilities>(6, setCaps);
      set<CookieChannel>(7, setCookies);
      set<{ url: string | null }>(8, (v) => setProxy(v.url ?? ""));
      set<number | null>(9, (cap) => setCapKb(cap ? Math.round(cap / 1024) : 0));
      set<ProbeTierSettings>(10, setTiers);
    })();
  }, []);

  useEffect(() => {
    loadAll();
    return () => {
      loadSeq.current += 1;
    };
  }, [loadAll]);

  const loadWarning = (...keys: SettingKey[]) =>
    isBlocked(...keys) && (
      <div className="ph-settings__loadfail" data-testid={`load-failed-${keys[0]}`}>
        <InlineNotification
          kind="warning"
          lowContrast
          hideCloseButton
          title={t("settings.loadFailed")}
          subtitle={t("settings.loadFailed.detail")}
        />
        <Button kind="ghost" size="sm" onClick={loadAll}>
          {t("settings.retryLoad")}
        </Button>
      </div>
    );

  // ---- filename template live preview (debounced)
  useEffect(() => {
    if (!template.trim()) {
      setTemplatePreview("");
      setTemplateError(t("settings.template.empty"));
      return;
    }
    // A reply that arrives after a newer request was sent is stale and must not win (L-57).
    let current = true;
    const timer = window.setTimeout(() => {
      api
        .previewFilenameTemplate(template)
        .then((preview) => {
          if (!current) return;
          setTemplatePreview(preview);
          setTemplateError(null);
        })
        .catch((err) => {
          if (!current) return;
          setTemplatePreview("");
          setTemplateError(String(err));
        });
    }, 200);
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [template]);

  const run = useCallback(
    async (label: string, action: () => Promise<unknown>): Promise<boolean> => {
      try {
        await action();
        toasts.success(t("settings.saved"), label);
        return true;
      } catch (err) {
        toasts.error(t("settings.saveFailed", { setting: label }), err);
        return false;
      }
    },
    [toasts],
  );

  return (
    <div className="ph-scroll" data-testid="settings-page">
      <div className="ph-settings">
        <Section title={t("settings.libraryRoot")} hint={t("settings.libraryRoot.hint")}>
          {loadWarning("libraryRoot")}
          <div className="ph-settings__row">
            <TextInput
              id="library-root"
              className="ph-settings__path"
              labelText={t("settings.libraryRoot")}
              value={libraryRoot}
              onChange={(e) => setLibraryRoot(e.target.value)}
            />
            <FolderPicker label={t("settings.choose")} kind="tertiary" onPicked={(p) => setLibraryRoot(p)} />
            <Button
              kind="primary"
              disabled={isBlocked("libraryRoot")}
              onClick={() =>
                void run(t("settings.libraryRoot"), async () => {
                  const applied = await api.setLibraryRoot(libraryRoot.trim());
                  setLibraryRoot(applied);
                  const notes = await api.takeLibraryRootWarnings().catch(() => [] as string[]);
                  notes.forEach((note) => toasts.warning(t("libraryRoot.warning"), note));
                })
              }
            >
              {t("settings.apply")}
            </Button>
          </div>
        </Section>

        <Section title={t("settings.transfers")} hint={t("settings.transfers.hint")}>
          {loadWarning("activeLimit", "bandwidth")}
          <div className="ph-settings__row">
            <NumberInput
              id="active-limit"
              label={t("queue.footer.activeLimit")}
              min={1}
              max={16}
              step={1}
              value={activeLimit}
              onChange={(_, { value }) => setActiveLimit(Number(value) || 1)}
            />
            <Button kind="tertiary" disabled={isBlocked("activeLimit")} onClick={() => void run(t("queue.footer.activeLimit"), () => api.setActiveJobLimit(activeLimit))}>
              {t("settings.apply")}
            </Button>
          </div>
          <div className="ph-settings__row" style={{ marginTop: "1rem" }}>
            <NumberInput
              id="bandwidth-cap"
              label={t("settings.bandwidth")}
              helperText={t("settings.bandwidth.hint")}
              min={0}
              step={64}
              value={capKb}
              onChange={(_, { value }) => setCapKb(Math.max(0, Number(value) || 0))}
            />
            <Button
              kind="tertiary"
              disabled={isBlocked("bandwidth")}
              onClick={() => void run(t("settings.bandwidth"), () => api.setGlobalBandwidthCap(capKb > 0 ? capKb * 1024 : null))}
            >
              {t("settings.apply")}
            </Button>
          </div>
        </Section>

        <Section title={t("settings.quality")} hint={t("settings.quality.hint")}>
          {loadWarning("quality", "audioOnly")}
          <div className="ph-settings__row">
            <Dropdown
              id="quality-max-height"
              titleText={t("settings.quality.maxHeight")}
              label={t("settings.quality.maxHeight")}
              items={HEIGHTS}
              itemToString={(i) => i?.label ?? ""}
              selectedItem={HEIGHTS.find((h) => h.id === policy.max_height) ?? { id: policy.max_height, label: `${policy.max_height}p` }}
              onChange={({ selectedItem }) => selectedItem && setPolicy((p) => ({ ...p, max_height: selectedItem.id }))}
            />
            <Checkbox
              id="quality-prefer-mp4"
              labelText={t("settings.quality.preferMp4")}
              checked={policy.prefer_mp4}
              onChange={(_, { checked }) => setPolicy((p) => ({ ...p, prefer_mp4: checked }))}
            />
            <Button kind="tertiary" disabled={isBlocked("quality")} onClick={() => void run(t("settings.quality"), () => api.setDefaultQualityPolicy(policy))}>
              {t("settings.apply")}
            </Button>
          </div>
          <div style={{ marginTop: "1rem" }}>
            <Toggle
              id="audio-only"
              labelText={t("settings.audioOnly")}
              labelA={t("common.off")}
              labelB={t("common.on")}
              toggled={audioOnly}
              disabled={isBlocked("audioOnly")}
              onToggle={(checked) => {
                const previous = audioOnly;
                setAudioOnly(checked);
                void run(t("settings.audioOnly"), () => api.setAudioOnlyMode(checked)).then((saved) => {
                  if (!saved) setAudioOnly(previous);
                });
              }}
            />
          </div>
        </Section>

        <Section title={t("settings.template")} hint={t("settings.template.hint")}>
          {loadWarning("template")}
          <div className="ph-settings__row">
            <TextInput
              id="filename-template"
              className="ph-settings__path"
              labelText={t("settings.template")}
              value={template}
              invalid={templateError != null}
              invalidText={templateError ?? ""}
              onChange={(e) => setTemplate(e.target.value)}
            />
            <Button
              kind="tertiary"
              disabled={templateError != null || isBlocked("template")}
              onClick={() => void run(t("settings.template"), () => api.setFilenameTemplate(template))}
            >
              {t("settings.apply")}
            </Button>
          </div>
          {templatePreview && (
            <div className="ph-settings__preview" data-testid="template-preview">
              {t("settings.template.preview")}: {templatePreview}
            </div>
          )}
        </Section>

        <Section title={t("probe.mediaOptions")} hint={t("settings.extras.hint")}>
          {loadWarning("extras")}
          {!caps.ffmpeg_available && (
            <p className="ph-muted" style={{ marginBottom: "0.75rem" }}>
              {t("probe.media.noFfmpeg")}
            </p>
          )}
          <div className="ph-settings__grid">
            {MEDIA_FIELDS.map((f) => (
              <Checkbox
                key={f.key}
                id={`default-extra-${f.key}`}
                labelText={t(f.label)}
                checked={extras[f.key]}
                disabled={f.needsFfmpeg && !caps.ffmpeg_available}
                onChange={(_, { checked }) => setExtras((prev) => ({ ...prev, [f.key]: checked }))}
              />
            ))}
          </div>
          <div style={{ marginTop: "1rem" }}>
            <Button kind="tertiary" disabled={isBlocked("extras")} onClick={() => void run(t("probe.mediaOptions"), () => api.setDefaultMediaExtras(extras))}>
              {t("settings.apply")}
            </Button>
          </div>
        </Section>

        <Section id="settings-cookies" title={t("settings.cookies")} hint={t("settings.cookies.hint")}>
          {loadWarning("cookies")}
          <div className="ph-settings__row">
            <Dropdown
              id="cookies-browser"
              titleText={t("settings.cookies.browser")}
              label={t("settings.cookies.browser")}
              items={browsers}
              itemToString={(i) => i?.label ?? ""}
              selectedItem={browsers.find((b) => b.id === (cookies.cookies_from_browser ?? "")) ?? browsers[0]}
              onChange={({ selectedItem }) =>
                selectedItem && setCookies((c) => ({ ...c, cookies_from_browser: selectedItem.id || null }))
              }
            />
            <TextInput
              id="cookies-file"
              className="ph-settings__path"
              labelText={t("settings.cookies.file")}
              helperText={t("settings.cookies.fileHint")}
              placeholder="/home/you/cookies.txt"
              value={cookies.cookie_file ?? ""}
              onChange={(e) => setCookies((c) => ({ ...c, cookie_file: e.target.value || null }))}
            />
            <Button kind="tertiary" disabled={isBlocked("cookies")} onClick={() => void run(t("settings.cookies"), () => api.setCookieChannel(cookies))}>
              {t("settings.apply")}
            </Button>
          </div>
        </Section>

        <Section title={t("settings.proxy")} hint={t("settings.proxy.hint")}>
          {loadWarning("proxy")}
          <div className="ph-settings__row">
            <TextInput
              id="proxy-url"
              className="ph-settings__path"
              labelText={t("settings.proxy")}
              placeholder="socks5://127.0.0.1:9050"
              value={proxy}
              onChange={(e) => setProxy(e.target.value)}
            />
            <Button kind="tertiary" disabled={isBlocked("proxy")} onClick={() => void run(t("settings.proxy"), () => api.setProxySettings({ url: proxy.trim() || null }))}>
              {t("settings.apply")}
            </Button>
          </div>
        </Section>

        <Section title={t("settings.tiers")} hint={t("settings.tiers.hint")}>
          {loadWarning("tiers")}
          <div className="ph-settings__grid">
            <Toggle
              id="tier-generic"
              labelText={t("settings.tiers.generic")}
              labelA={t("common.off")}
              labelB={t("common.on")}
              toggled={tiers.allow_generic}
              onToggle={(checked) => setTiers((s) => ({ ...s, allow_generic: checked }))}
            />
            <Toggle
              id="tier-sniff"
              labelText={t("settings.tiers.sniff")}
              labelA={t("common.off")}
              labelB={t("common.on")}
              toggled={tiers.allow_page_sniff}
              onToggle={(checked) => setTiers((s) => ({ ...s, allow_page_sniff: checked }))}
            />
            <Toggle
              id="tier-direct"
              labelText={t("settings.tiers.direct")}
              labelA={t("common.off")}
              labelB={t("common.on")}
              toggled={tiers.allow_direct_fallback}
              onToggle={(checked) => setTiers((s) => ({ ...s, allow_direct_fallback: checked }))}
            />
          </div>
          <div className="ph-settings__row" style={{ marginTop: "1rem" }}>
            <NumberInput
              id="probe-timeout"
              label={t("settings.tiers.timeout")}
              min={5}
              max={600}
              step={5}
              value={tiers.probe_timeout_secs}
              onChange={(_, { value }) => setTiers((s) => ({ ...s, probe_timeout_secs: Number(value) || 45 }))}
            />
            <Button kind="tertiary" disabled={isBlocked("tiers")} onClick={() => void run(t("settings.tiers"), () => api.setProbeTierSettings(tiers))}>
              {t("settings.apply")}
            </Button>
          </div>
        </Section>

        <Section title={t("settings.notifications")}>
          <Toggle
            id="desktop-notifications"
            labelText={t("settings.notifications.desktop")}
            labelA={t("common.off")}
            labelB={t("common.on")}
            toggled={notificationsEnabled}
            onToggle={onNotificationsChange}
          />
        </Section>

        <Section title={t("toolchain.title")} hint={t("toolchain.hint")}>
          <ToolchainPanel onStatus={onToolchainStatus} />
        </Section>
      </div>
    </div>
  );
}
