import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { SettingsPage } from "./SettingsPage";
import { ToastProvider } from "@/state/useToasts";
import { installFakeIpc, toolchainStatus, type CommandHandler } from "@/test/mockApi";

function setup(overrides: Record<string, CommandHandler> = {}) {
  const extras = { write_subs: false, embed_subs: false, write_chapters: false, embed_chapters: false, write_thumbnail: false, embed_thumbnail: false, write_metadata: false, embed_metadata: false, remux: false };
  const ipc = installFakeIpc({
    get_library_root: () => "/home/u/lib",
    get_active_job_limit: () => 3,
    get_audio_only_mode: () => false,
    get_default_quality_policy: () => ({ max_height: 1080, prefer_mp4: true }),
    get_filename_template: () => "%(title)s [%(id)s].%(ext)s",
    get_default_media_extras: () => extras,
    get_media_extras_capabilities: () => ({ ffmpeg_available: true, embed_disabled_reason: null }),
    get_cookie_channel: () => ({ cookies_from_browser: null, cookie_file: null }),
    get_proxy_settings: () => ({ url: null }),
    get_global_bandwidth_cap: () => null,
    get_probe_tier_settings: () => ({ allow_generic: true, allow_page_sniff: true, allow_direct_fallback: true, probe_timeout_secs: 45 }),
    get_sidecar_update_settings: () => ({ check_on_launch: false, channel: "stable", toolchain_pin: null, plugin_pin: null }),
    get_toolchain_status: () => toolchainStatus(),
    preview_filename_template: () => "Example Video Title [dQw4w9WgXcQ].mp4",
    set_library_root: (a) => a.path,
    set_active_job_limit: () => undefined,
    set_global_bandwidth_cap: () => undefined,
    set_default_quality_policy: () => undefined,
    set_audio_only_mode: () => undefined,
    set_filename_template: () => undefined,
    set_default_media_extras: () => undefined,
    set_cookie_channel: () => undefined,
    set_proxy_settings: () => undefined,
    set_probe_tier_settings: () => undefined,
    set_sidecar_update_settings: () => undefined,
    install_toolchain_latest: () => ({ outcome: "applied", applied: ["toolchain"] }),
    check_sidecar_updates: () => ({
      toolchain: { component: "toolchain", installed_version: "2026.08.19", available_version: "2026.10.01", pinned_version: null, update_available: true },
      plugin: { component: "plugin", installed_version: "0.1.0", available_version: "0.1.0", pinned_version: null, update_available: false },
    }),
    pin_sidecar_version: () => undefined,
    unpin_sidecar_version: () => undefined,
    rollback_sidecar: () => undefined,
    pick_folder: () => ({ path: "/home/u/Videos", method: "portal" }),
    ...overrides,
  });
  const onNotifications = (() => {
    const calls: boolean[] = [];
    const fn = (v: boolean) => void calls.push(v);
    return Object.assign(fn, { calls });
  })();
  render(
    <ToastProvider>
      <SettingsPage notificationsEnabled onNotificationsChange={onNotifications} onToolchainStatus={() => undefined} />
    </ToastProvider>,
  );
  return { ipc, onNotifications };
}

describe("SettingsPage: no dead buttons", () => {
  it("every Apply button persists through its command", async () => {
    const { ipc } = setup();
    const user = userEvent.setup();
    await screen.findByTestId("template-preview");
    const apply = screen.getAllByRole("button", { name: "Apply" });
    expect(apply.length).toBeGreaterThanOrEqual(9);
    for (const button of apply) {
      await user.click(button);
    }
    await waitFor(() => {
      const called = new Set(ipc.calls.map((c) => c.cmd));
      for (const cmd of [
        "set_library_root",
        "set_active_job_limit",
        "set_global_bandwidth_cap",
        "set_default_quality_policy",
        "set_filename_template",
        "set_default_media_extras",
        "set_cookie_channel",
        "set_proxy_settings",
        "set_probe_tier_settings",
      ]) {
        expect(called, `${cmd} was never invoked`).toContain(cmd);
      }
    });
  });

  it("toggles persist immediately", async () => {
    const { ipc, onNotifications } = setup();
    const user = userEvent.setup();
    await screen.findByTestId("template-preview");
    await user.click(screen.getByRole("switch", { name: /audio only/i }));
    await waitFor(() => expect(ipc.callsTo("set_audio_only_mode")).toEqual([{ enabled: true }]));
    await user.click(screen.getByRole("switch", { name: /desktop notifications/i }));
    expect(onNotifications.calls).toEqual([false]);
    await user.click(screen.getByRole("switch", { name: /check for updates on launch/i }));
    await waitFor(() => expect(ipc.callsTo("set_sidecar_update_settings")).toHaveLength(1));
  });

  it("toolchain actions: install, check, pin, unpin and roll back", async () => {
    const { ipc } = setup();
    const user = userEvent.setup();
    await screen.findByText("2026.08.19");
    await user.click(screen.getByRole("button", { name: "Install / update yt-dlp" }));
    await waitFor(() => expect(ipc.callsTo("install_toolchain_latest")).toHaveLength(1));

    await user.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByTestId("toolchain-update-result")).toHaveTextContent("2026.10.01");

    await user.type(screen.getByLabelText("Pin yt-dlp version"), "2026.08.19");
    const pinButtons = screen.getAllByRole("button", { name: "Pin" });
    await user.click(pinButtons[0]);
    await waitFor(() => expect(ipc.callsTo("pin_sidecar_version")).toEqual([{ component: "toolchain", version: "2026.08.19" }]));

    await user.click(screen.getAllByRole("button", { name: "Unpin" })[0]); // plugin row has an empty pin
    await waitFor(() => expect(ipc.callsTo("unpin_sidecar_version")).toEqual([{ component: "plugin" }]));

    await user.click(screen.getAllByRole("button", { name: "Roll back" })[0]);
    await waitFor(() => expect(ipc.callsTo("rollback_sidecar")).toEqual([{ component: "toolchain" }]));
  });

  it("filename template preview tracks input and blocks an empty template", async () => {
    setup();
    const user = userEvent.setup();
    expect(await screen.findByTestId("template-preview")).toHaveTextContent("Example Video Title");
    const input = screen.getByLabelText("Filename template");
    await user.clear(input);
    expect(await screen.findByText("The template cannot be empty.")).toBeInTheDocument();
    const applyButtons = screen.getAllByRole("button", { name: "Apply" });
    expect(applyButtons.some((b) => (b as HTMLButtonElement).disabled)).toBe(true);
  });

  it("surfaces toolchain gaps with an actionable message", async () => {
    installFakeIpc({
      get_library_root: () => "/x", get_active_job_limit: () => 3, get_audio_only_mode: () => false,
      get_default_quality_policy: () => ({ max_height: 1080, prefer_mp4: true }), get_filename_template: () => "t",
      get_default_media_extras: () => ({}), get_media_extras_capabilities: () => ({ ffmpeg_available: false, embed_disabled_reason: "ffmpeg not found" }),
      get_cookie_channel: () => ({ cookies_from_browser: null, cookie_file: null }), get_proxy_settings: () => ({ url: null }),
      get_global_bandwidth_cap: () => null,
      get_probe_tier_settings: () => ({ allow_generic: true, allow_page_sniff: true, allow_direct_fallback: true, probe_timeout_secs: 45 }),
      get_sidecar_update_settings: () => ({ check_on_launch: false, channel: "stable", toolchain_pin: null, plugin_pin: null }),
      preview_filename_template: () => "x",
      get_toolchain_status: () => toolchainStatus({ ytdlp: null, ytdlp_source: null, js_runtime: null, issues: ["yt-dlp was not found. Install the `yt-dlp` package."] }),
    });
    render(
      <ToastProvider>
        <SettingsPage notificationsEnabled onNotificationsChange={() => undefined} onToolchainStatus={() => undefined} />
      </ToastProvider>,
    );
    expect(await screen.findByText(/yt-dlp was not found/)).toBeInTheDocument();
    expect(screen.getAllByText("Missing").length).toBeGreaterThan(0);
  });

  it.each([
    [true, false],
    [false, true],
  ])("shows browser impersonation (available=%s) in the Toolchain panel", async (available, hintShown) => {
    installFakeIpc({
      get_library_root: () => "/x", get_active_job_limit: () => 3, get_audio_only_mode: () => false,
      get_default_quality_policy: () => ({ max_height: 1080, prefer_mp4: true }), get_filename_template: () => "t",
      get_default_media_extras: () => ({}), get_media_extras_capabilities: () => ({ ffmpeg_available: true, embed_disabled_reason: null }),
      get_cookie_channel: () => ({ cookies_from_browser: null, cookie_file: null }), get_proxy_settings: () => ({ url: null }),
      get_global_bandwidth_cap: () => null,
      get_probe_tier_settings: () => ({ allow_generic: true, allow_page_sniff: true, allow_direct_fallback: true, probe_timeout_secs: 45 }),
      get_sidecar_update_settings: () => ({ check_on_launch: false, channel: "stable", toolchain_pin: null, plugin_pin: null }),
      preview_filename_template: () => "x",
      get_toolchain_status: () => toolchainStatus({ impersonation_available: available }),
    });
    render(
      <ToastProvider>
        <SettingsPage notificationsEnabled onNotificationsChange={() => undefined} onToolchainStatus={() => undefined} />
      </ToastProvider>,
    );
    expect(await screen.findByText("Browser impersonation")).toBeInTheDocument();
    expect(screen.queryByText(/Needs curl_cffi/) !== null).toBe(hintShown);
  });

  describe("load failures", () => {
    it("a setting that failed to load keeps its Apply disabled while the others stay usable (M-60)", async () => {
      const { ipc } = setup({ get_cookie_channel: () => Promise.reject("cookie store unreadable") });
      const user = userEvent.setup();
      await screen.findByTestId("template-preview");
      expect(await screen.findByTestId("load-failed-cookies")).toBeInTheDocument();

      const cookiesSection = document.getElementById("settings-cookies")!;
      expect(within(cookiesSection).getByRole("button", { name: "Apply" })).toBeDisabled();

      const proxySection = screen.getByLabelText("Proxy").closest("section")!;
      const proxyApply = within(proxySection).getByRole("button", { name: "Apply" });
      expect(proxyApply).toBeEnabled();
      await user.click(proxyApply);
      await waitFor(() => expect(ipc.callsTo("set_proxy_settings")).toHaveLength(1));
      expect(ipc.callsTo("set_cookie_channel")).toHaveLength(0);
    });

    it("Retry reloads the failed setting and re-enables Apply", async () => {
      let fail = true;
      setup({
        get_cookie_channel: () =>
          fail ? Promise.reject("busy") : { cookies_from_browser: "firefox", cookie_file: null },
      });
      const user = userEvent.setup();
      await screen.findByTestId("load-failed-cookies");
      fail = false;
      await user.click(within(screen.getByTestId("load-failed-cookies")).getByRole("button", { name: "Retry" }));
      await waitFor(() => expect(screen.queryByTestId("load-failed-cookies")).not.toBeInTheDocument());
      expect(within(document.getElementById("settings-cookies")!).getByRole("button", { name: "Apply" })).toBeEnabled();
    });

    it("an older template preview reply cannot overwrite a newer one (L-57)", async () => {
      const replies = new Map<string, (value: string) => void>();
      setup({
        preview_filename_template: (args) =>
          new Promise<string>((resolve) => {
            replies.set(String(args.template), resolve);
          }),
      });
      const user = userEvent.setup();
      const input = await screen.findByLabelText("Filename template");
      await waitFor(() => expect(replies.size).toBe(1)); // preview of the loaded template
      await user.type(input, "A");
      await waitFor(() => expect(replies.size).toBe(2));
      await user.type(input, "B");
      await waitFor(() => expect(replies.size).toBe(3));
      const [, older, newer] = [...replies.values()];
      newer("newer preview");
      expect(await screen.findByTestId("template-preview")).toHaveTextContent("newer preview");
      older("older preview");
      await new Promise((resolve) => setTimeout(resolve, 20));
      expect(screen.getByTestId("template-preview")).toHaveTextContent("newer preview");
    });

    it("the audio-only switch reverts when saving fails (L-58)", async () => {
      setup({ set_audio_only_mode: () => Promise.reject("disk full") });
      const user = userEvent.setup();
      await screen.findByTestId("template-preview");
      const toggle = screen.getByRole("switch", { name: /audio only/i });
      await user.click(toggle);
      await waitFor(() => expect(screen.getByRole("switch", { name: /audio only/i })).not.toBeChecked());
    });
  });

  describe("Toolchain panel", () => {
    it("does not write default pins back when the update settings failed to load (M-59)", async () => {
      const { ipc } = setup({ get_sidecar_update_settings: () => Promise.reject("settings.json unreadable") });
      const user = userEvent.setup();
      expect(await screen.findByTestId("toolchain-settings-failed")).toBeInTheDocument();
      await screen.findByText("2026.08.19");

      const toggle = screen.getByRole("switch", { name: /check for updates on launch/i });
      expect(toggle).toBeDisabled();
      await user.click(toggle);
      for (const pin of screen.getAllByRole("button", { name: "Unpin" })) expect(pin).toBeDisabled();
      expect(ipc.callsTo("set_sidecar_update_settings")).toHaveLength(0);
      expect(ipc.callsTo("unpin_sidecar_version")).toHaveLength(0);
    });

    it("Retry loads the settings and enables saving", async () => {
      let fail = true;
      setup({
        get_sidecar_update_settings: () =>
          fail ? Promise.reject("busy") : { check_on_launch: true, toolchain_pin: "2026.01.01", plugin_pin: null },
      });
      const user = userEvent.setup();
      await user.click(await within(await screen.findByTestId("toolchain-settings-failed")).findByRole("button", { name: "Retry" }).then((b) => { fail = false; return b; }));
      await waitFor(() => expect(screen.queryByTestId("toolchain-settings-failed")).not.toBeInTheDocument());
      expect(screen.getByRole("switch", { name: /check for updates on launch/i })).toBeEnabled();
      expect(screen.getByLabelText("Pin yt-dlp version")).toHaveValue("2026.01.01");
    });
  });
});
