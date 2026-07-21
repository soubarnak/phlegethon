using System.Text.Json.Serialization;

namespace Phlegethon.Services;

/// <summary>Write/embed toggles for subtitles, chapters, thumbnails, metadata, and remux.</summary>
public sealed class MediaExtrasOptions
{
    [JsonPropertyName("write_subs")]
    public bool WriteSubs { get; set; }

    [JsonPropertyName("embed_subs")]
    public bool EmbedSubs { get; set; }

    [JsonPropertyName("write_chapters")]
    public bool WriteChapters { get; set; }

    [JsonPropertyName("embed_chapters")]
    public bool EmbedChapters { get; set; }

    [JsonPropertyName("write_thumbnail")]
    public bool WriteThumbnail { get; set; }

    [JsonPropertyName("embed_thumbnail")]
    public bool EmbedThumbnail { get; set; }

    [JsonPropertyName("write_metadata")]
    public bool WriteMetadata { get; set; }

    [JsonPropertyName("embed_metadata")]
    public bool EmbedMetadata { get; set; }

    [JsonPropertyName("remux")]
    public bool Remux { get; set; }
}

public sealed class MediaExtrasCapabilities
{
    [JsonPropertyName("ffmpeg_available")]
    public bool FfmpegAvailable { get; set; }

    [JsonPropertyName("embed_disabled_reason")]
    public string? EmbedDisabledReason { get; set; }
}

/// <summary>Single Core cookie auth channel for extractor probe/download.</summary>
public sealed class CookieChannel
{
    [JsonPropertyName("cookies_from_browser")]
    public string? CookiesFromBrowser { get; set; }

    [JsonPropertyName("cookie_file")]
    public string? CookieFile { get; set; }
}

/// <summary>Global HTTP(S)/SOCKS proxy for HTTP transfers and extractor toolchain.</summary>
public sealed class ProxySettings
{
    [JsonPropertyName("url")]
    public string? Url { get; set; }
}

/// <summary>Sidecar update manager settings (toolchain + plugin).</summary>
public sealed class SidecarUpdateSettings
{
    [JsonPropertyName("check_on_launch")]
    public bool CheckOnLaunch { get; set; }

    [JsonPropertyName("toolchain_pin")]
    public string? ToolchainPin { get; set; }

    [JsonPropertyName("plugin_pin")]
    public string? PluginPin { get; set; }
}

public sealed class SidecarComponentStatus
{
    [JsonPropertyName("component")]
    public string Component { get; set; } = "";

    [JsonPropertyName("installed_version")]
    public string? InstalledVersion { get; set; }

    [JsonPropertyName("available_version")]
    public string? AvailableVersion { get; set; }

    [JsonPropertyName("pinned_version")]
    public string? PinnedVersion { get; set; }

    [JsonPropertyName("update_available")]
    public bool UpdateAvailable { get; set; }
}

public sealed class SidecarUpdateCheckResult
{
    [JsonPropertyName("toolchain")]
    public SidecarComponentStatus Toolchain { get; set; } = new();

    [JsonPropertyName("plugin")]
    public SidecarComponentStatus Plugin { get; set; } = new();
}

public sealed class SidecarApplyResult
{
    [JsonPropertyName("outcome")]
    public string Outcome { get; set; } = "";

    [JsonPropertyName("applied")]
    public List<string> Applied { get; set; } = new();
}
