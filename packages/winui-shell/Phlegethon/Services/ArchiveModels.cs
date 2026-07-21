using System.Text.Json.Serialization;

namespace Phlegethon.Services;

/// <summary>Download Archive entry — single source of truth for History (#61).</summary>
public class ArchiveEntry
{
    [JsonPropertyName("media_id")]
    public string MediaId { get; init; } = "";

    [JsonPropertyName("title")]
    public string Title { get; init; } = "";

    [JsonPropertyName("url")]
    public string Url { get; init; } = "";

    [JsonPropertyName("completed_at_unix_secs")]
    public ulong CompletedAtUnixSecs { get; init; }

    /// <summary>Output file path (completed) or job destination folder (failed).</summary>
    [JsonPropertyName("destination")]
    public string Destination { get; init; } = "";

    [JsonPropertyName("job_id")]
    public string JobId { get; init; } = "";

    [JsonPropertyName("outcome")]
    public string Outcome { get; init; } = "Completed";
}
