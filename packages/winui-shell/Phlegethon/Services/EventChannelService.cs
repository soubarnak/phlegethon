using System;
using System.Collections.Generic;
using System.Text.Json;
using System.Threading;
using System.Threading.Channels;
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

// ---------------------------------------------------------------------------
// Data models matching Rust structs
// ---------------------------------------------------------------------------

public class MediaItem
{
    public string Id { get; init; } = "";
    public string Title { get; init; } = "";
    public string Url { get; init; } = "";
    public List<Quality> Qualities { get; init; } = new();
}

public class Quality
{
    public string Id { get; init; } = "";
    public string Label { get; init; } = "";
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
    private readonly Channel<JsonElement> _eventChannel;
    private CancellationTokenSource? _cts;
    private Task? _eventLoopTask;
    private Task? _processLoopTask;

    public EventChannelService()
    {
        _core = new CoreHandle();
        _eventChannel = Channel.CreateBounded<JsonElement>(new BoundedChannelOptions(256)
        {
            FullMode = BoundedChannelFullMode.DropOldest,
        });

        // Set a default library root
        var appData = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        var libraryRoot = System.IO.Path.Combine(appData, "Phlegethon", "Library");
        _core.SetLibraryRoot(libraryRoot);
    }

    public CoreHandle Core => _core;

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------

    public event EventHandler<ProbeResultEventArgs>? OnProbeResult;
    public event EventHandler<JobProgressEventArgs>? OnJobProgress;
    public event EventHandler<JobCompletedEventArgs>? OnJobCompleted;
    public event EventHandler<JobFailedEventArgs>? OnJobFailed;

    // -----------------------------------------------------------------------
    // URL enqueue
    // -----------------------------------------------------------------------

    /// <summary>
    /// Enqueue a URL. If classified as probe_then_confirm, the probe-result
    /// event will fire asynchronously.
    /// </summary>
    public void EnqueueUrl(string url)
    {
        var json = _core.Enqueue(url);
        var doc = JsonDocument.Parse(json);
        var type = doc.RootElement.GetProperty("type").GetString();

        if (type == "enqueue_direct")
        {
            // Job created directly — process_pending will handle it
        }
        else if (type == "probe_then_confirm")
        {
            // The core's probe() call is already queued via enqueue -> classification
            // We need to explicitly probe now
            _ = Task.Run(() =>
            {
                try
                {
                    var probeJson = _core.Probe(url);
                    var probeDoc = JsonDocument.Parse(probeJson);
                    // The event will come through the event channel
                }
                catch (Exception ex)
                {
                    System.Diagnostics.Debug.WriteLine($"Probe error: {ex.Message}");
                }
            });
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
                            await _eventChannel.Writer.WriteAsync(ev, token);
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
                OnProbeResult?.Invoke(this, new ProbeResultEventArgs
                {
                    Url = ev.GetProperty("url").GetString() ?? "",
                    Items = DeserializeItems(ev.GetProperty("items")),
                    SourceName = ev.TryGetProperty("source_name", out var sn) ? sn.GetString() : null,
                });
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
