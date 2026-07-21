using System.Collections.Generic;
using System.Text.Json.Serialization;

namespace Phlegethon.Services;

/// <summary>Extension → shell hand-off request (v1 envelope).</summary>
public sealed class CompanionRequest
{
    [JsonPropertyName("v")]
    public uint Version { get; set; }

    [JsonPropertyName("op")]
    public string Op { get; set; } = "";

    [JsonPropertyName("request_id")]
    public string RequestId { get; set; } = "";

    [JsonPropertyName("page_url")]
    public string PageUrl { get; set; } = "";

    [JsonPropertyName("media_url")]
    public string? MediaUrl { get; set; }

    [JsonPropertyName("session")]
    public CompanionSession? Session { get; set; }

    public string EffectiveUrl()
    {
        if (!string.IsNullOrWhiteSpace(MediaUrl))
        {
            return MediaUrl;
        }
        return PageUrl;
    }
}

public sealed class CompanionSession
{
    [JsonPropertyName("cookies")]
    public List<CompanionCookie> Cookies { get; set; } = new();
}

public sealed class CompanionCookie
{
    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    [JsonPropertyName("value")]
    public string Value { get; set; } = "";

    [JsonPropertyName("domain")]
    public string Domain { get; set; } = "";

    [JsonPropertyName("path")]
    public string Path { get; set; } = "/";

    [JsonPropertyName("secure")]
    public bool Secure { get; set; }

    [JsonPropertyName("expiration_date")]
    public long ExpirationDate { get; set; }
}

public sealed class CompanionResponse
{
    [JsonPropertyName("v")]
    public uint Version { get; set; } = 1;

    [JsonPropertyName("request_id")]
    public string RequestId { get; set; } = "";

    [JsonPropertyName("ok")]
    public bool Ok { get; set; }

    [JsonPropertyName("error")]
    public string? Error { get; set; }

    [JsonPropertyName("result")]
    public CompanionEnqueueResult? Result { get; set; }

    public static CompanionResponse Ok(string requestId, CompanionEnqueueResult result) =>
        new()
        {
            RequestId = requestId,
            Ok = true,
            Result = result,
        };

    public static CompanionResponse Err(string requestId, string error) =>
        new()
        {
            RequestId = requestId,
            Ok = false,
            Error = error,
        };
}

[JsonPolymorphic(TypeDiscriminatorPropertyName = "type")]
[JsonDerivedType(typeof(CompanionEnqueueDirect), "enqueue_direct")]
[JsonDerivedType(typeof(CompanionProbeThenConfirm), "probe_then_confirm")]
public abstract class CompanionEnqueueResult;

public sealed class CompanionEnqueueDirect : CompanionEnqueueResult
{
    [JsonPropertyName("job_id")]
    public string JobId { get; set; } = "";

    [JsonPropertyName("destination")]
    public string Destination { get; set; } = "";
}

public sealed class CompanionProbeThenConfirm : CompanionEnqueueResult
{
    [JsonPropertyName("url")]
    public string Url { get; set; } = "";
}

public sealed class CompanionIntentEventArgs : EventArgs
{
    public string Url { get; init; } = "";
    public CompanionEnqueueResult Result { get; init; } = new CompanionProbeThenConfirm();
}
