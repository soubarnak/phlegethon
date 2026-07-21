namespace Phlegethon.Services;

using System.Text.Json.Serialization;

/// <summary>Match for a URL already active or queued in Core.</summary>
public sealed class ActiveJobMatch
{
    [JsonPropertyName("job_id")]
    public string JobId { get; init; } = "";

    [JsonPropertyName("url")]
    public string Url { get; init; } = "";

    [JsonPropertyName("status")]
    public string Status { get; init; } = "";

    [JsonPropertyName("title")]
    public string Title { get; init; } = "";
}
