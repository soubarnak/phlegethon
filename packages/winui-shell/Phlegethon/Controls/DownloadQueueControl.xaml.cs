using System;
using System.Collections.ObjectModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Phlegethon.Controls;

/// <summary>
/// View model for a single row in the download queue.
/// </summary>
public class DownloadItemViewModel
{
    public string JobId { get; set; } = "";
    public string Title { get; set; } = "Untitled";
    public string Subtitle { get; set; } = "Queued";
    /// <summary>Job Destination folder when known (Save to… / Core destination).</summary>
    public string Destination { get; set; } = "";
    public string StatusIcon { get; set; } = "⋯";
    public string ActionLabel { get; set; } = "Pause";
    public double ProgressPercent { get; set; } = 0;
    public DownloadStatus Status { get; set; } = DownloadStatus.Queued;
}

public enum DownloadStatus
{
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed,
}

/// <summary>
/// Dense-row download queue control (variant A).
/// </summary>
public sealed partial class DownloadQueueControl : UserControl
{
    private readonly ObservableCollection<DownloadItemViewModel> _items = new();

    public DownloadQueueControl()
    {
        this.InitializeComponent();
        DownloadList.ItemsSource = _items;
    }

    /// <summary>
    /// Add or update a job row.
    /// </summary>
    public void AddOrUpdateJob(string jobId, string title, string subtitle, DownloadStatus status, double progress, string? destination = null)
    {
        // Find existing item
        foreach (var item in _items)
        {
            if (item.JobId == jobId)
            {
                item.Title = title;
                item.Subtitle = subtitle;
                item.Status = status;
                item.ProgressPercent = progress;
                if (destination != null)
                {
                    item.Destination = destination;
                }
                UpdateVisualStatus(item);
                return;
            }
        }

        // Add new
        var newItem = new DownloadItemViewModel
        {
            JobId = jobId,
            Title = title,
            Subtitle = subtitle,
            Destination = destination ?? "",
            Status = status,
            ProgressPercent = progress,
        };
        UpdateVisualStatus(newItem);
        _items.Add(newItem);

        UpdateEmptyState();
    }

    /// <summary>
    /// Update job progress from background event.
    /// </summary>
    public void UpdateJobProgress(string jobId, ulong bytesDownloaded, ulong? totalBytes)
    {
        foreach (var item in _items)
        {
            if (item.JobId == jobId)
            {
                item.Status = DownloadStatus.Downloading;
                var progressText = FormatProgress(bytesDownloaded, totalBytes);
                item.Subtitle = string.IsNullOrEmpty(item.Destination)
                    ? progressText
                    : $"{progressText} · {item.Destination}";
                if (totalBytes.HasValue && totalBytes.Value > 0)
                {
                    item.ProgressPercent = Math.Min(100.0, (double)bytesDownloaded / totalBytes.Value * 100.0);
                }
                else
                {
                    item.ProgressPercent = 0;
                }
                UpdateVisualStatus(item);
                return;
            }
        }
    }

    /// <summary>
    /// Mark a job completed.
    /// </summary>
    public void MarkJobCompleted(string jobId, string path)
    {
        foreach (var item in _items)
        {
            if (item.JobId == jobId)
            {
                item.Status = DownloadStatus.Completed;
                var dest = string.IsNullOrEmpty(item.Destination)
                    ? (System.IO.Path.GetDirectoryName(path) ?? "")
                    : item.Destination;
                item.Subtitle = string.IsNullOrEmpty(dest)
                    ? $"Downloaded to {path}"
                    : $"Downloaded to {path} · Save to {dest}";
                item.ProgressPercent = 100;
                UpdateVisualStatus(item);
                return;
            }
        }

        // Completed event arrived before enqueue/confirm inserted a row.
        var folder = System.IO.Path.GetDirectoryName(path) ?? "";
        AddOrUpdateJob(
            jobId,
            System.IO.Path.GetFileName(path) ?? "Download",
            string.IsNullOrEmpty(folder)
                ? $"Downloaded to {path}"
                : $"Downloaded to {path} · Save to {folder}",
            DownloadStatus.Completed,
            100,
            folder);
    }

    /// <summary>
    /// Mark a job failed.
    /// </summary>
    public void MarkJobFailed(string jobId, string error)
    {
        foreach (var item in _items)
        {
            if (item.JobId == jobId)
            {
                item.Status = DownloadStatus.Failed;
                item.Subtitle = $"Failed: {error}";
                UpdateVisualStatus(item);
                return;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Event handlers
    // -----------------------------------------------------------------------

    private void OnItemAction(object sender, RoutedEventArgs e)
    {
        var button = (Button)sender;
        var item = (DownloadItemViewModel)button.DataContext;

        switch (item.Status)
        {
            case DownloadStatus.Downloading:
            case DownloadStatus.Queued:
                // Pause
                item.Status = DownloadStatus.Paused;
                UpdateVisualStatus(item);
                break;
            case DownloadStatus.Paused:
                // Resume
                item.Status = DownloadStatus.Downloading;
                UpdateVisualStatus(item);
                break;
            case DownloadStatus.Completed:
            case DownloadStatus.Failed:
                // Retry or clear
                _items.Remove(item);
                UpdateEmptyState();
                break;
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    private static string FormatProgress(ulong bytesDownloaded, ulong? totalBytes)
    {
        if (totalBytes.HasValue && totalBytes.Value > 0)
        {
            var downloaded = FormatBytes(bytesDownloaded);
            var total = FormatBytes(totalBytes.Value);
            var percent = (double)bytesDownloaded / totalBytes.Value * 100.0;
            return $"{downloaded} / {total} ({percent:F1}%)";
        }
        else
        {
            return $"{FormatBytes(bytesDownloaded)} downloaded";
        }
    }

    private static string FormatBytes(ulong bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:F1} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024.0 * 1024):F1} MB";
        return $"{bytes / (1024.0 * 1024 * 1024):F2} GB";
    }

    private void UpdateVisualStatus(DownloadItemViewModel item)
    {
        switch (item.Status)
        {
            case DownloadStatus.Queued:
                item.StatusIcon = "⋯";
                item.ActionLabel = "Pause";
                break;
            case DownloadStatus.Downloading:
                item.StatusIcon = "↓";
                item.ActionLabel = "Pause";
                break;
            case DownloadStatus.Paused:
                item.StatusIcon = "⏸";
                item.ActionLabel = "Resume";
                break;
            case DownloadStatus.Completed:
                item.StatusIcon = "✓";
                item.ActionLabel = "Clear";
                break;
            case DownloadStatus.Failed:
                item.StatusIcon = "✗";
                item.ActionLabel = "Retry";
                break;
        }
    }

    private void UpdateEmptyState()
    {
        EmptyStateText.Visibility = _items.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }
}
