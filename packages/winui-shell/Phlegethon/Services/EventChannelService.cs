using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;

namespace Phlegethon.Services;

// ---------------------------------------------------------------------------
// Event argument types
// ---------------------------------------------------------------------------

public class ProbeResultEventArgs : EventArgs
{
    public string Url { get; init; } = "";
    public List<MediaItem> Items { get; init; } = new();
    public string? SourceName { get; init; }
}

public class ProbeFailedEventArgs : EventArgs
{
    public string Url { get; init; } = "";
    public string Error { get; init; } = "";
}

public class JobProgressEventArgs : EventArgs
{
    public string JobId { get; init; } = "";
    public ulong BytesDownloaded { get; init; }
    public ulong? TotalBytes { get; init; }
}

public class JobCompletedEventArgs : EventArgs
{
    public string JobId { get; init; } = "";
    public string Path { get; init; } = "";
}

public class JobFailedEventArgs : EventArgs
{
    public string JobId { get; init; } = "";
    public string Error { get; init; } = "";
}

public class JobQueuedEventArgs : EventArgs
{
    public string JobId { get; init; } = "";
    public string Url { get; init; } = "";
    public string Destination { get; init; } = "";
    public string Title { get; init; } = "";
}

public class JobDiscardedEventArgs : EventArgs
{
    public string JobId { get; init; } = "";
}

public class JobLifecycleEventArgs : EventArgs
{
    public string JobId { get; init; } = "";
}

public class JobSnapshot
{
    [JsonPropertyName("job_id")]
    public string JobId { get; init; } = "";

    [JsonPropertyName("url")]
    public string Url { get; init; } = "";

    [JsonPropertyName("destination")]
    public string Destination { get; init; } = "";

    [JsonPropertyName("status")]
    public string Status { get; init; } = "";

    [JsonPropertyName("bytes_downloaded")]
    public ulong BytesDownloaded { get; init; }

    [JsonPropertyName("total_bytes")]
    public ulong? TotalBytes { get; init; }

    [JsonPropertyName("title")]
    public string Title { get; init; } = "";
}

// ---------------------------------------------------------------------------
// Data models matching Rust structs
// ---------------------------------------------------------------------------

public class MediaItem
{
    public string Id { get; init; } = "";
    public string Title { get; init; } = "";
    public string Url { get; init; } = "";
    public List<Quality> Qualities { get; init; } = new();
    public bool Archived { get; init; }
}

public class Quality
{
    [JsonPropertyName("id")]
    public string Id { get; init; } = "";

    [JsonPropertyName("label")]
    public string Label { get; init; } = "";

    [JsonPropertyName("size_bytes")]
    public ulong? SizeBytes { get; init; }
}

public class ProbeResult
{
    public string Url { get; init; } = "";
    public List<MediaItem> Items { get; init; } = new();
    public string? SourceName { get; init; }
}

// ---------------------------------------------------------------------------
// Event Channel Service
// ---------------------------------------------------------------------------

/// <summary>
/// Bridges the Download Core event stream to the WinUI dispatcher.
///
/// Owns a background task that drains core events via `core_drain_events_json`,
/// writes them into a `Channel<CoreEvent>`, and dispatches typed C# events
/// to subscribers on the UI thread.
///
/// Also owns the process_pending loop so downloads progress autonomously.
/// </summary>
public class EventChannelService : IDisposable
{
    private readonly CoreHandle _core;
    private CancellationTokenSource? _cts;
    private Task? _eventLoopTask;
    private Task? _processLoopTask;

    public EventChannelService()
    {
        _core = new CoreHandle();

        // Set a default library root
        var appData = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        var libraryRoot = System.IO.Path.Combine(appData, "Phlegethon", "Library");
        var sidecarRoot = System.IO.Path.Combine(appData, "Phlegethon", "Sidecar");
        _core.SetLibraryRoot(libraryRoot);
        _core.SetSidecarRoot(sidecarRoot);
    }

    public CoreHandle Core => _core;

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------

    public event EventHandler<ProbeResultEventArgs>? OnProbeResult;
    public event EventHandler<ProbeFailedEventArgs>? OnProbeFailed;
    public event EventHandler<JobProgressEventArgs>? OnJobProgress;
    public event EventHandler<JobCompletedEventArgs>? OnJobCompleted;
    public event EventHandler<JobFailedEventArgs>? OnJobFailed;
    public event EventHandler<JobQueuedEventArgs>? OnJobQueued;
    public event EventHandler<JobDiscardedEventArgs>? OnJobDiscarded;
    public event EventHandler<JobLifecycleEventArgs>? OnJobPaused;
    public event EventHandler<JobLifecycleEventArgs>? OnJobResumed;
    public event EventHandler<JobLifecycleEventArgs>? OnJobRetried;
    public event EventHandler<string>? OnProcessPendingError;

    // -----------------------------------------------------------------------
    // URL enqueue
    // -----------------------------------------------------------------------

    public IReadOnlyList<string> SplitUrls(string text) => _core.SplitUrls(text);

    public string? NormalizeUrl(string url) => _core.NormalizeUrl(url);

    public ActiveJobMatch? FindActiveJobForUrl(string url) => _core.FindActiveJobForUrl(url);

    /// <summary>
    /// Enqueue a URL for Chrome Companion hand-off. Returns outcome for NMH ack;
    /// probe path still requires explicit user confirm (no auto-confirm).
    /// </summary>
    public CompanionEnqueueResult HandleCompanionEnqueue(string url)
    {
        var json = _core.Enqueue(url);
        DispatchEnqueueResult(url, json);
        var doc = JsonDocument.Parse(json);
        var type = doc.RootElement.GetProperty("type").GetString();

        if (type == "enqueue_direct")
        {
            var jobId = doc.RootElement.GetProperty("job_id").GetString() ?? "";
            var destination = doc.RootElement.TryGetProperty("destination", out var destEl)
                ? destEl.GetString() ?? ""
                : "";
            return new CompanionEnqueueDirect
            {
                JobId = jobId,
                Destination = destination,
            };
        }

        var probeUrl = doc.RootElement.GetProperty("url").GetString() ?? url;
        StartProbeInBackground(probeUrl);
        return new CompanionProbeThenConfirm { Url = probeUrl };
    }

    private void StartProbeInBackground(string url)
    {
        _ = Task.Run(() =>
        {
            try
            {
                var probeJson = _core.Probe(url);
                DispatchProbeJson(url, probeJson);
            }
            catch (Exception ex)
            {
                OnProbeFailed?.Invoke(this, new ProbeFailedEventArgs
                {
                    Url = url,
                    Error = ex.Message,
                });
            }
        });
    }

    private void DispatchProbeJson(string fallbackUrl, string probeJson)
    {
        using var probeDoc = JsonDocument.Parse(probeJson);
        var root = probeDoc.RootElement;
        var urlFromResult = root.TryGetProperty("url", out var urlEl)
            ? urlEl.GetString() ?? fallbackUrl
            : fallbackUrl;
        var sourceName = root.TryGetProperty("source_name", out var snEl)
            ? snEl.GetString()
            : null;
        var items = root.TryGetProperty("items", out var itemsEl)
            ? DeserializeItems(itemsEl)
            : new List<MediaItem>();

        OnProbeResult?.Invoke(this, new ProbeResultEventArgs
        {
            Url = urlFromResult,
            Items = items,
            SourceName = sourceName,
        });
    }

    /// <summary>
    /// Enqueue a URL. If classified as probe_then_confirm, the probe-result
    /// event will fire asynchronously.
    /// </summary>
    public void EnqueueUrl(string url, string? destination = null)
    {
        var json = destination != null
            ? _core.Enqueue(url, destination)
            : _core.Enqueue(url);
        DispatchEnqueueResult(url, json);
    }

    private void DispatchEnqueueResult(string url, string json)
    {
        var doc = JsonDocument.Parse(json);
        var type = doc.RootElement.GetProperty("type").GetString();

        if (type == "enqueue_direct")
        {
            var jobId = doc.RootElement.GetProperty("job_id").GetString() ?? "";
            var destination = doc.RootElement.TryGetProperty("destination", out var destEl)
                ? destEl.GetString() ?? ""
                : "";
            OnJobQueued?.Invoke(this, new JobQueuedEventArgs
            {
                JobId = jobId,
                Url = url,
                Destination = destination,
                Title = TitleFromUrl(url),
            });
        }
        else if (type == "probe_then_confirm")
        {
            StartProbeInBackground(url);
        }
    }

    public void ClearPendingProbe(string url) => _core.ClearPendingProbe(url);

    public void PauseJob(string jobId) => _core.PauseJob(jobId);

    public void ResumeJob(string jobId) => _core.ResumeJob(jobId);

    public void RetryJob(string jobId) => _core.RetryJob(jobId);

    public IReadOnlyList<JobSnapshot> ListJobSnapshots()
    {
        var json = _core.ListJobSnapshotsJson();
        return System.Text.Json.JsonSerializer.Deserialize<List<JobSnapshot>>(json)
            ?? new List<JobSnapshot>();
    }

    /// <summary>
    /// Fetch exact formats for one probe item and update Core pending probe state.
    /// </summary>
    public List<Quality> EnrichProbeItemQualities(string probeUrl, string itemId)
    {
        var json = _core.EnrichProbeItemQualities(probeUrl, itemId);
        return System.Text.Json.JsonSerializer.Deserialize<List<Quality>>(
            json,
            new System.Text.Json.JsonSerializerOptions { PropertyNameCaseInsensitive = true })
            ?? new List<Quality>();
    }

    /// <summary>
    /// Confirm a probe selection. Returns job-queued notifications for the UI thread.
    /// </summary>
    public IReadOnlyList<JobQueuedEventArgs> ConfirmProbeSelection(string selectionJson)
    {
        var resultJson = _core.ConfirmProbeSelection(selectionJson);
        var notifications = BuildJobQueuedNotifications(selectionJson, resultJson);
        return notifications;
    }

    private IReadOnlyList<JobQueuedEventArgs> BuildJobQueuedNotifications(string selectionJson, string resultJson)
    {
        var notifications = new List<JobQueuedEventArgs>();
        try
        {
            using var selectionDoc = JsonDocument.Parse(selectionJson);
            var url = selectionDoc.RootElement.TryGetProperty("url", out var urlEl)
                ? urlEl.GetString() ?? ""
                : "";
            var fallbackDestination = selectionDoc.RootElement.TryGetProperty("destination", out var destEl)
                ? destEl.GetString() ?? ""
                : "";

            var snapshots = ListJobSnapshots().ToDictionary(j => j.JobId);

            using var resultDoc = JsonDocument.Parse(resultJson);
            if (resultDoc.RootElement.ValueKind == JsonValueKind.Array)
            {
                var titles = new List<string>();
                if (selectionDoc.RootElement.TryGetProperty("items", out var itemsEl)
                    && itemsEl.ValueKind == JsonValueKind.Array)
                {
                    foreach (var item in itemsEl.EnumerateArray())
                    {
                        var title = item.TryGetProperty("title", out var titleEl)
                            ? titleEl.GetString() ?? ""
                            : "";
                        titles.Add(title);
                    }
                }

                var index = 0;
                foreach (var idEl in resultDoc.RootElement.EnumerateArray())
                {
                    var jobId = idEl.GetString() ?? "";
                    if (string.IsNullOrEmpty(jobId)) continue;
                    var title = index < titles.Count && !string.IsNullOrWhiteSpace(titles[index])
                        ? titles[index]
                        : TitleFromUrl(url);
                    var destination = snapshots.TryGetValue(jobId, out var snap)
                        ? snap.Destination
                        : fallbackDestination;
                    notifications.Add(new JobQueuedEventArgs
                    {
                        JobId = jobId,
                        Url = url,
                        Destination = destination,
                        Title = title,
                    });
                    index++;
                }
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"BuildJobQueuedNotifications error: {ex.Message}");
        }

        return notifications;
    }

    /// <summary>
    /// Preview Core-resolved Job Destination for a probe selection JSON
    /// (same shape as confirm). Returns <c>path_display</c>, or null on failure.
    /// </summary>
    public string? PreviewProbeDestination(string selectionJson)
    {
        try
        {
            var previewJson = _core.PreviewProbeDestination(selectionJson);
            using var doc = JsonDocument.Parse(previewJson);
            if (doc.RootElement.TryGetProperty("path_display", out var pathEl))
            {
                return pathEl.GetString();
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"Preview destination error: {ex.Message}");
        }

        return null;
    }

    /// <summary>
    /// Discard a job via Core (caller must have confirmed with the user).
    /// </summary>
    public void DiscardJob(string jobId)
    {
        _core.DiscardJob(jobId);
    }

    public void ReorderJob(int fromIndex, int toIndex)
    {
        _core.ReorderJob(fromIndex, toIndex);
    }

    public void SetActiveJobLimit(uint limit)
    {
        _core.SetActiveJobLimit(limit);
    }

    public bool GetAudioOnlyMode() => _core.GetAudioOnlyMode();

    public void SetAudioOnlyMode(bool enabled) => _core.SetAudioOnlyMode(enabled);

    public MediaExtrasOptions GetDefaultMediaExtras()
    {
        var json = _core.GetDefaultMediaExtrasJson();
        return System.Text.Json.JsonSerializer.Deserialize<MediaExtrasOptions>(json)
            ?? new MediaExtrasOptions();
    }

    public void SetDefaultMediaExtras(MediaExtrasOptions options)
    {
        var json = System.Text.Json.JsonSerializer.Serialize(options);
        _core.SetDefaultMediaExtrasJson(json);
    }

    public MediaExtrasCapabilities GetMediaExtrasCapabilities()
    {
        var json = _core.GetMediaExtrasCapabilitiesJson();
        return System.Text.Json.JsonSerializer.Deserialize<MediaExtrasCapabilities>(json)
            ?? new MediaExtrasCapabilities();
    }

    public CookieChannel GetCookieChannel()
    {
        var json = _core.GetCookieChannelJson();
        return System.Text.Json.JsonSerializer.Deserialize<CookieChannel>(json)
            ?? new CookieChannel();
    }

    public void SetCookieChannel(CookieChannel channel)
    {
        var json = System.Text.Json.JsonSerializer.Serialize(channel);
        _core.SetCookieChannelJson(json);
    }

    public ProxySettings GetProxySettings()
    {
        var json = _core.GetProxySettingsJson();
        return System.Text.Json.JsonSerializer.Deserialize<ProxySettings>(json)
            ?? new ProxySettings();
    }

    public void SetProxySettings(ProxySettings settings)
    {
        var json = System.Text.Json.JsonSerializer.Serialize(settings);
        _core.SetProxySettingsJson(json);
    }

    public SidecarUpdateSettings GetSidecarUpdateSettings()
    {
        var json = _core.GetSidecarUpdateSettingsJson();
        return System.Text.Json.JsonSerializer.Deserialize<SidecarUpdateSettings>(json)
            ?? new SidecarUpdateSettings();
    }

    public void SetSidecarUpdateSettings(SidecarUpdateSettings settings)
    {
        var json = System.Text.Json.JsonSerializer.Serialize(settings);
        _core.SetSidecarUpdateSettingsJson(json);
    }

    public SidecarUpdateCheckResult CheckSidecarUpdates()
    {
        var json = _core.CheckSidecarUpdatesJson();
        return System.Text.Json.JsonSerializer.Deserialize<SidecarUpdateCheckResult>(json)
            ?? new SidecarUpdateCheckResult();
    }

    public SidecarApplyResult ApplySidecarUpdates()
    {
        var json = _core.ApplySidecarUpdatesJson();
        return System.Text.Json.JsonSerializer.Deserialize<SidecarApplyResult>(json)
            ?? new SidecarApplyResult();
    }

    public void PinSidecarVersion(string component, string version) =>
        _core.PinSidecarVersion(component, version);

    public void UnpinSidecarVersion(string component) =>
        _core.UnpinSidecarVersion(component);

    public void RollbackSidecar(string component) =>
        _core.RollbackSidecar(component);

    public string GetFilenameTemplate() => _core.GetFilenameTemplate();

    public void SetFilenameTemplate(string template) => _core.SetFilenameTemplate(template);

    public IReadOnlyList<ArchiveEntry> ListHistory() => _core.ListHistory();

    public IReadOnlyList<ArchiveEntry> ListHistoryByOutcome(string outcome) =>
        _core.ListHistoryByOutcome(outcome);

    public void RemoveHistoryEntry(string mediaId) => _core.RemoveHistoryEntry(mediaId);

    public int HistoryLen() => _core.HistoryLen();

    public string? SelectDefaultQuality(IReadOnlyList<Quality> qualities)
    {
        var payload = System.Text.Json.JsonSerializer.Serialize(
            qualities.Select(q => new
            {
                id = q.Id,
                label = q.Label,
                size_bytes = (ulong?)null,
                container = (string?)null,
            }));
        return _core.SelectDefaultQuality(payload);
    }

    private static string TitleFromUrl(string url)
    {
        try
        {
            var uri = new Uri(url);
            var segments = uri.AbsolutePath.Split('/', StringSplitOptions.RemoveEmptyEntries);
            if (segments.Length > 0)
            {
                return Uri.UnescapeDataString(segments[^1]);
            }
            return uri.Host;
        }
        catch
        {
            return "Download";
        }
    }

    // -----------------------------------------------------------------------
    // Event loop
    // -----------------------------------------------------------------------

    /// <summary>
    /// Start the background event drain + process_pending loops.
    /// </summary>
    public async Task StartEventLoopAsync()
    {
        _cts = new CancellationTokenSource();
        var token = _cts.Token;

        _ = Task.Run(() =>
        {
            try
            {
                _ = _core.ApplySidecarUpdatesOnLaunchJson();
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"Sidecar on-launch error: {ex.Message}");
            }
        }, token);

        // Event drain loop (~10 Hz)
        _eventLoopTask = Task.Run(async () =>
        {
            while (!token.IsCancellationRequested)
            {
                try
                {
                    var eventsJson = _core.DrainEvents();
                    if (!string.IsNullOrEmpty(eventsJson) && eventsJson != "[]")
                    {
                        var doc = JsonDocument.Parse(eventsJson);
                        foreach (var ev in doc.RootElement.EnumerateArray())
                        {
                            DispatchEvent(ev);
                        }
                    }
                }
                catch (OperationCanceledException)
                {
                    break;
                }
                catch (Exception ex)
                {
                    System.Diagnostics.Debug.WriteLine($"Event drain error: {ex.Message}");
                }

                await Task.Delay(100, token);
            }
        }, token);

        // Process pending loop (~5 Hz)
        _processLoopTask = Task.Run(async () =>
        {
            while (!token.IsCancellationRequested)
            {
                try
                {
                    _core.ProcessPending();
                }
                catch (Exception ex)
                {
                    System.Diagnostics.Debug.WriteLine($"ProcessPending error: {ex.Message}");
                    OnProcessPendingError?.Invoke(this, ex.Message);
                }
                await Task.Delay(200, token);
            }
        }, token);
    }

    // -----------------------------------------------------------------------
    // Event dispatch
    // -----------------------------------------------------------------------

    private void DispatchEvent(JsonElement ev)
    {
        var type = ev.GetProperty("type").GetString();
        switch (type)
        {
            case "ProbeResult":
            case "ProbeFailed":
                // Probe UI is driven from StartProbeInBackground (Probe FFI return JSON).
                break;

            case "JobProgress":
                OnJobProgress?.Invoke(this, new JobProgressEventArgs
                {
                    JobId = ev.GetProperty("job_id").GetString() ?? "",
                    BytesDownloaded = ev.GetProperty("bytes_downloaded").GetUInt64(),
                    TotalBytes = ev.TryGetProperty("total_bytes", out var tb) && tb.ValueKind != System.Text.Json.JsonValueKind.Null
                        ? tb.GetUInt64()
                        : null,
                });
                break;

            case "JobCompleted":
                OnJobCompleted?.Invoke(this, new JobCompletedEventArgs
                {
                    JobId = ev.GetProperty("job_id").GetString() ?? "",
                    Path = ev.GetProperty("path").GetString() ?? "",
                });
                break;

            case "JobFailed":
                OnJobFailed?.Invoke(this, new JobFailedEventArgs
                {
                    JobId = ev.GetProperty("job_id").GetString() ?? "",
                    Error = ev.GetProperty("error").GetString() ?? "",
                });
                break;

            case "JobDiscarded":
                OnJobDiscarded?.Invoke(this, new JobDiscardedEventArgs
                {
                    JobId = ev.GetProperty("job_id").GetString() ?? "",
                });
                break;

            case "JobPaused":
                OnJobPaused?.Invoke(this, new JobLifecycleEventArgs
                {
                    JobId = ev.GetProperty("job_id").GetString() ?? "",
                });
                break;

            case "JobResumed":
                OnJobResumed?.Invoke(this, new JobLifecycleEventArgs
                {
                    JobId = ev.GetProperty("job_id").GetString() ?? "",
                });
                break;

            case "JobRetried":
                OnJobRetried?.Invoke(this, new JobLifecycleEventArgs
                {
                    JobId = ev.GetProperty("job_id").GetString() ?? "",
                });
                break;
        }
    }

    private static List<MediaItem> DeserializeItems(JsonElement itemsEl)
    {
        var items = new List<MediaItem>();
        foreach (var item in itemsEl.EnumerateArray())
        {
            var mediaItem = new MediaItem
            {
                Id = item.GetProperty("id").GetString() ?? "",
                Title = item.GetProperty("title").GetString() ?? "",
                Url = item.GetProperty("url").GetString() ?? "",
                Qualities = DeserializeQualities(item.GetProperty("qualities")),
                Archived = item.TryGetProperty("archived", out var archivedEl)
                    && archivedEl.GetBoolean(),
            };
            items.Add(mediaItem);
        }
        return items;
    }

    private static List<Quality> DeserializeQualities(JsonElement qs)
    {
        var qualities = new List<Quality>();
        foreach (var q in qs.EnumerateArray())
        {
            qualities.Add(new Quality
            {
                Id = q.GetProperty("id").GetString() ?? "",
                Label = q.GetProperty("label").GetString() ?? "",
            });
        }
        return qualities;
    }

    // -----------------------------------------------------------------------
    // Cleanup
    // -----------------------------------------------------------------------

    public void Dispose()
    {
        _cts?.Cancel();
        _eventLoopTask?.Wait(TimeSpan.FromSeconds(2));
        _processLoopTask?.Wait(TimeSpan.FromSeconds(2));
        _core.Dispose();
        _cts?.Dispose();
    }
}
