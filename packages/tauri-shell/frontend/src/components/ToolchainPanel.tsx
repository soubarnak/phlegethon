import { useCallback, useEffect, useState } from "react";
import {
  Button,
  InlineLoading,
  InlineNotification,
  StructuredListBody,
  StructuredListCell,
  StructuredListHead,
  StructuredListRow,
  StructuredListWrapper,
  Tag,
  TextInput,
  Toggle,
} from "@carbon/react";
import { api } from "@/tauri/api";
import type {
  SidecarComponent,
  SidecarUpdateCheckResult,
  SidecarUpdateSettings,
  ToolInfo,
  ToolchainStatus,
} from "@/tauri/types";
import { t } from "@/lib/i18n";
import { useToasts } from "@/state/useToasts";

interface ToolchainPanelProps {
  /** Reported up so the header can show a health tag. */
  onStatus?: (status: ToolchainStatus | null) => void;
}

function toolRow(label: string, info: ToolInfo | null, missingText: string) {
  return (
    <StructuredListRow key={label}>
      <StructuredListCell noWrap>{label}</StructuredListCell>
      <StructuredListCell>
        <div className="ph-toolchain__row-cell">
          <Tag size="sm" type={info ? "green" : "red"}>
            {info ? t("toolchain.found") : t("toolchain.missing")}
          </Tag>
          {info ? (
            <>
              {info.version && <span>{info.version}</span>}
              <span className="ph-toolchain__path">{info.path}</span>
            </>
          ) : (
            <span className="ph-muted">{missingText}</span>
          )}
        </div>
      </StructuredListCell>
    </StructuredListRow>
  );
}

export function ToolchainPanel({ onStatus }: ToolchainPanelProps) {
  const toasts = useToasts();
  const [status, setStatus] = useState<ToolchainStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [installing, setInstalling] = useState(false);
  const [checking, setChecking] = useState(false);
  const [update, setUpdate] = useState<SidecarUpdateCheckResult | null>(null);
  const [settings, setSettings] = useState<SidecarUpdateSettings>({
    check_on_launch: true,
    channel: "stable",
    toolchain_pin: null,
    plugin_pin: null,
  });
  // False until get_sidecar_update_settings succeeds. Saving or pinning from the
  // placeholder defaults would clear the user's pins (M-59).
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [toolchainPin, setToolchainPin] = useState("");
  const [pluginPin, setPluginPin] = useState("");

  const load = useCallback(
    async (force = false) => {
      setLoading(true);
      try {
        const next = await api.getToolchainStatus(force);
        setStatus(next);
        onStatus?.(next);
      } catch (err) {
        toasts.error(t("toolchain.statusFailed"), err);
        onStatus?.(null);
      } finally {
        setLoading(false);
      }
    },
    [onStatus, toasts],
  );

  const loadSettings = useCallback(() => {
    api
      .getSidecarUpdateSettings()
      .then((s) => {
        setSettings(s);
        setToolchainPin(s.toolchain_pin ?? "");
        setPluginPin(s.plugin_pin ?? "");
        setSettingsLoaded(true);
      })
      .catch((err) => {
        setSettingsLoaded(false);
        toasts.error(t("toolchain.settingsFailed"), err);
      });
  }, [toasts]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    loadSettings();
    // Load once; a retry is explicit.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const saveSettings = async (next: SidecarUpdateSettings) => {
    try {
      await api.setSidecarUpdateSettings(next);
      setSettings(next);
    } catch (err) {
      toasts.error(t("toolchain.saveFailed"), err);
    }
  };

  const install = async () => {
    setInstalling(true);
    try {
      const result = await api.installToolchainLatest();
      switch (result.outcome) {
        case "applied":
          toasts.success(t("toolchain.installed"));
          break;
        case "nothing_to_apply":
          toasts.info(t("toolchain.upToDate"));
          break;
        case "blocked_mid_transfer":
          toasts.warning(t("toolchain.blocked"));
          break;
        default:
          toasts.info(t("toolchain.deferred"));
      }
      await load(true);
    } catch (err) {
      toasts.error(t("toolchain.installFailed"), err);
    } finally {
      setInstalling(false);
    }
  };

  const check = async () => {
    setChecking(true);
    try {
      setUpdate(await api.checkSidecarUpdates());
    } catch (err) {
      toasts.error(t("toolchain.checkFailed"), err);
    } finally {
      setChecking(false);
    }
  };

  const pin = async (component: SidecarComponent, version: string) => {
    const trimmed = version.trim();
    try {
      if (trimmed) {
        await api.pinSidecarVersion(component, trimmed);
        toasts.success(t("toolchain.pinned", { version: trimmed }));
      } else {
        await api.unpinSidecarVersion(component);
        toasts.info(t("toolchain.unpinned"));
      }
      setSettings(await api.getSidecarUpdateSettings());
    } catch (err) {
      toasts.error(t("toolchain.pinFailed"), err);
    }
  };

  const rollback = async (component: SidecarComponent) => {
    try {
      await api.rollbackSidecar(component);
      toasts.success(t("toolchain.rolledBack"));
      await load(true);
    } catch (err) {
      toasts.error(t("toolchain.rollbackFailed"), err);
    }
  };

  return (
    <div data-testid="toolchain-panel">
      {loading && !status && <InlineLoading description={t("toolchain.detecting")} />}
      {status && (
        <>
          <StructuredListWrapper isCondensed aria-label={t("toolchain.title")}>
            <StructuredListHead>
              <StructuredListRow head>
                <StructuredListCell head>{t("toolchain.col.tool")}</StructuredListCell>
                <StructuredListCell head>{t("toolchain.col.state")}</StructuredListCell>
              </StructuredListRow>
            </StructuredListHead>
            <StructuredListBody>
              {toolRow("yt-dlp", status.ytdlp, t("toolchain.ytdlpMissing"))}
              {toolRow("ffmpeg", status.ffmpeg, t("toolchain.ffmpegMissing"))}
              {toolRow("ffprobe", status.ffprobe, t("toolchain.ffmpegMissing"))}
              <StructuredListRow>
                <StructuredListCell noWrap>{t("toolchain.jsRuntime")}</StructuredListCell>
                <StructuredListCell>
                  <div className="ph-toolchain__row-cell">
                    <Tag size="sm" type={status.js_runtime?.supported ? "green" : status.js_runtime ? "warm-gray" : "red"}>
                      {status.js_runtime ? (status.js_runtime.supported ? t("toolchain.found") : t("toolchain.tooOld")) : t("toolchain.missing")}
                    </Tag>
                    {status.js_runtime ? (
                      <>
                        <span>
                          {status.js_runtime.kind} {status.js_runtime.version ?? ""}
                        </span>
                        <span className="ph-toolchain__path">{status.js_runtime.path}</span>
                      </>
                    ) : (
                      <span className="ph-muted">{t("toolchain.jsMissing")}</span>
                    )}
                  </div>
                </StructuredListCell>
              </StructuredListRow>
              <StructuredListRow>
                <StructuredListCell noWrap>yt-dlp-ejs</StructuredListCell>
                <StructuredListCell>
                  <Tag size="sm" type={status.ejs === "missing" ? "red" : status.ejs === "unknown" ? "gray" : "green"}>
                    {t(`toolchain.ejs.${status.ejs}`)}
                  </Tag>
                </StructuredListCell>
              </StructuredListRow>
              <StructuredListRow>
                <StructuredListCell noWrap>{t("toolchain.impersonation")}</StructuredListCell>
                <StructuredListCell>
                  <Tag size="sm" type={status.impersonation_available ? "green" : "warm-gray"}>
                    {status.impersonation_available ? t("toolchain.found") : t("toolchain.missing")}
                  </Tag>
                  {!status.impersonation_available && (
                    <span className="ph-muted"> {t("toolchain.impersonationMissing")}</span>
                  )}
                </StructuredListCell>
              </StructuredListRow>
              <StructuredListRow>
                <StructuredListCell noWrap>{t("toolchain.platform")}</StructuredListCell>
                <StructuredListCell>
                  <span>
                    {status.platform.os} · {status.platform.arch} · {status.platform.libc}
                  </span>
                  {status.ytdlp_source && (
                    <span className="ph-muted"> · {t(`toolchain.source.${status.ytdlp_source}`)}</span>
                  )}
                </StructuredListCell>
              </StructuredListRow>
            </StructuredListBody>
          </StructuredListWrapper>

          {status.issues.map((issue) => (
            <InlineNotification
              key={issue}
              kind="warning"
              lowContrast
              hideCloseButton
              title={issue}
              style={{ maxWidth: "100%" }}
            />
          ))}
        </>
      )}

      <div className="ph-toolchain__actions">
        <Button kind="primary" size="md" disabled={installing} onClick={() => void install()}>
          {installing ? <InlineLoading description={t("toolchain.installing")} /> : t("toolchain.install")}
        </Button>
        <Button kind="tertiary" size="md" disabled={checking} onClick={() => void check()}>
          {checking ? <InlineLoading description={t("toolchain.checking")} /> : t("toolchain.check")}
        </Button>
        <Button kind="ghost" size="md" disabled={loading} onClick={() => void load(true)}>
          {t("toolchain.refresh")}
        </Button>
      </div>

      {update && (
        <div style={{ marginTop: "1rem" }} data-testid="toolchain-update-result">
          {([update.toolchain, update.plugin] as const).map((c) => (
            <p key={c.component}>
              <strong>{c.component === "toolchain" ? "yt-dlp" : t("toolchain.plugins")}</strong>:{" "}
              {c.installed_version ?? t("toolchain.notInstalled")}
              {c.update_available && c.available_version
                ? ` → ${c.available_version}`
                : ` (${t("toolchain.upToDate")})`}
              {c.pinned_version ? ` · ${t("toolchain.pinnedTo", { version: c.pinned_version })}` : ""}
            </p>
          ))}
        </div>
      )}

      <div style={{ marginTop: "1.5rem", display: "grid", gap: "1rem", maxWidth: "40rem" }}>
        {!settingsLoaded && (
          <div data-testid="toolchain-settings-failed">
            <InlineNotification
              kind="warning"
              lowContrast
              hideCloseButton
              title={t("toolchain.settingsFailed")}
              subtitle={t("toolchain.settingsFailed.detail")}
            />
            <Button kind="ghost" size="sm" onClick={loadSettings}>
              {t("settings.retryLoad")}
            </Button>
          </div>
        )}
        <Toggle
          id="check-on-launch"
          labelText={t("toolchain.checkOnLaunch")}
          labelA={t("common.off")}
          labelB={t("common.on")}
          disabled={!settingsLoaded}
          toggled={settings.check_on_launch}
          onToggle={(checked) => void saveSettings({ ...settings, check_on_launch: checked })}
        />
        <Toggle
          id="toolchain-nightly"
          labelText={t("toolchain.nightlyChannel")}
          labelA={t("common.off")}
          labelB={t("common.on")}
          toggled={settings.channel === "nightly"}
          onToggle={(checked) =>
            void saveSettings({ ...settings, channel: checked ? "nightly" : "stable" })
          }
        />
        <div className="ph-settings__row">
          <TextInput
            id="toolchain-pin"
            labelText={t("toolchain.pinToolchain")}
            helperText={t("toolchain.pinHelper")}
            placeholder="2026.08.19"
            value={toolchainPin}
            onChange={(e) => setToolchainPin(e.target.value)}
          />
          <Button kind="tertiary" size="md" disabled={!settingsLoaded} onClick={() => void pin("toolchain", toolchainPin)}>
            {toolchainPin.trim() ? t("toolchain.pin") : t("toolchain.unpin")}
          </Button>
          <Button kind="danger--tertiary" size="md" onClick={() => void rollback("toolchain")}>
            {t("toolchain.rollback")}
          </Button>
        </div>
        <div className="ph-settings__row">
          <TextInput
            id="plugin-pin"
            labelText={t("toolchain.pinPlugin")}
            helperText={t("toolchain.pinHelper")}
            placeholder="0.1.0"
            value={pluginPin}
            onChange={(e) => setPluginPin(e.target.value)}
          />
          <Button kind="tertiary" size="md" disabled={!settingsLoaded} onClick={() => void pin("plugin", pluginPin)}>
            {pluginPin.trim() ? t("toolchain.pin") : t("toolchain.unpin")}
          </Button>
          <Button kind="danger--tertiary" size="md" onClick={() => void rollback("plugin")}>
            {t("toolchain.rollback")}
          </Button>
        </div>
      </div>
    </div>
  );
}
