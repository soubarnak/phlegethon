using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Phlegethon.Services;

namespace Phlegethon.Controls;

/// <summary>
/// Settings panel shown inside a ContentDialog from MainWindow.
/// </summary>
public sealed partial class SettingsDialog : UserControl
{
    public SettingsDialog()
    {
        InitializeComponent();
    }

    public void SetAudioOnlyModeDisplay(bool enabled) =>
        AudioOnlyModeBox.IsChecked = enabled;

    public void SetMediaExtrasDefaultsDisplay(MediaExtrasOptions options)
    {
        WriteSubsDefaultBox.IsChecked = options.WriteSubs;
        WriteThumbnailDefaultBox.IsChecked = options.WriteThumbnail;
    }

    public void SetCookieChannelDisplay(CookieChannel channel)
    {
        CookiesFromBrowserBox.Text = channel.CookiesFromBrowser ?? string.Empty;
        CookieFileBox.Text = channel.CookieFile ?? string.Empty;
    }

    public void SetProxySettingsDisplay(ProxySettings settings) =>
        ProxyUrlBox.Text = settings.Url ?? string.Empty;

    public void SetSidecarUpdateSettingsDisplay(SidecarUpdateSettings settings)
    {
        CheckSidecarUpdatesOnLaunchBox.IsChecked = settings.CheckOnLaunch;
        ToolchainPinBox.Text = settings.ToolchainPin ?? string.Empty;
        PluginPinBox.Text = settings.PluginPin ?? string.Empty;
    }

    public void SetSidecarUpdateStatus(string message) =>
        SidecarUpdateStatusText.Text = message;

    public void SetFilenameTemplateDisplay(string template) =>
        FilenameTemplateBox.Text = template;

    private void OnFilenameTemplateApply(object sender, RoutedEventArgs e)
    {
        var template = FilenameTemplateBox.Text.Trim();
        if (!string.IsNullOrEmpty(template))
            FilenameTemplateChanged?.Invoke(this, template);
    }

    private void OnMediaExtrasDefaultsApply(object sender, RoutedEventArgs e)
    {
        MediaExtrasDefaultsChanged?.Invoke(this, new MediaExtrasOptions
        {
            WriteSubs = WriteSubsDefaultBox.IsChecked == true,
            WriteThumbnail = WriteThumbnailDefaultBox.IsChecked == true,
        });
    }

    private void OnCookieChannelApply(object sender, RoutedEventArgs e)
    {
        var browser = CookiesFromBrowserBox.Text.Trim();
        var file = CookieFileBox.Text.Trim();
        CookieChannelChanged?.Invoke(this, new CookieChannel
        {
            CookiesFromBrowser = string.IsNullOrEmpty(browser) ? null : browser,
            CookieFile = string.IsNullOrEmpty(file) ? null : file,
        });
    }

    private void OnProxySettingsApply(object sender, RoutedEventArgs e)
    {
        var url = ProxyUrlBox.Text.Trim();
        ProxySettingsChanged?.Invoke(this, new ProxySettings
        {
            Url = string.IsNullOrEmpty(url) ? null : url,
        });
    }

    private void OnSidecarUpdateSettingsChanged(object sender, RoutedEventArgs e)
    {
        SidecarUpdateSettingsChanged?.Invoke(this, new SidecarUpdateSettings
        {
            CheckOnLaunch = CheckSidecarUpdatesOnLaunchBox.IsChecked == true,
            ToolchainPin = string.IsNullOrWhiteSpace(ToolchainPinBox.Text)
                ? null
                : ToolchainPinBox.Text.Trim(),
            PluginPin = string.IsNullOrWhiteSpace(PluginPinBox.Text)
                ? null
                : PluginPinBox.Text.Trim(),
        });
    }

    private void OnCheckSidecarUpdates(object sender, RoutedEventArgs e) =>
        CheckSidecarUpdatesRequested?.Invoke(this, EventArgs.Empty);

    private void OnApplySidecarUpdates(object sender, RoutedEventArgs e) =>
        ApplySidecarUpdatesRequested?.Invoke(this, EventArgs.Empty);

    private void OnPinToolchain(object sender, RoutedEventArgs e)
    {
        var version = ToolchainPinBox.Text.Trim();
        if (!string.IsNullOrEmpty(version))
            PinSidecarVersionRequested?.Invoke(this, ("toolchain", version));
    }

    private void OnRollbackToolchain(object sender, RoutedEventArgs e) =>
        RollbackSidecarRequested?.Invoke(this, "toolchain");

    private void OnPinPlugin(object sender, RoutedEventArgs e)
    {
        var version = PluginPinBox.Text.Trim();
        if (!string.IsNullOrEmpty(version))
            PinSidecarVersionRequested?.Invoke(this, ("plugin", version));
    }

    private void OnRollbackPlugin(object sender, RoutedEventArgs e) =>
        RollbackSidecarRequested?.Invoke(this, "plugin");

    private void OnAudioOnlyModeChanged(object sender, RoutedEventArgs e) =>
        AudioOnlyModeChanged?.Invoke(this, AudioOnlyModeBox.IsChecked == true);

    public event EventHandler<bool>? AudioOnlyModeChanged;
    public event EventHandler<MediaExtrasOptions>? MediaExtrasDefaultsChanged;
    public event EventHandler<CookieChannel>? CookieChannelChanged;
    public event EventHandler<ProxySettings>? ProxySettingsChanged;
    public event EventHandler<SidecarUpdateSettings>? SidecarUpdateSettingsChanged;
    public event EventHandler? CheckSidecarUpdatesRequested;
    public event EventHandler? ApplySidecarUpdatesRequested;
    public event EventHandler<(string Component, string Version)>? PinSidecarVersionRequested;
    public event EventHandler<string>? RollbackSidecarRequested;
    public event EventHandler<string>? FilenameTemplateChanged;
}
