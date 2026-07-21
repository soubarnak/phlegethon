using System;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using Phlegethon.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Phlegethon.Controls;

/// <summary>
/// View model for a single row in the download queue.
/// </summary>
public class DownloadItemViewModel : INotifyPropertyChanged
{
    private string _jobId = "";
    private string _title = "Untitled";
    private string _subtitle = "Queued";
    private string _destination = "";
    private string _statusIcon = "⋯";
    private string _actionLabel = "Pause";
    private Visibility _primaryActionVisible = Visibility.Visible;
    private double _progressPercent;
    private DownloadStatus _status = DownloadStatus.Queued;

    public event PropertyChangedEventHandler? PropertyChanged;

    public string JobId
    {
        get => _jobId;
        set => SetField(ref _jobId, value);
    }

    public string Title
    {
        get => _title;
        set => SetField(ref _title, value);
    }

    public string Subtitle
    {
        get => _subtitle;
        set => SetField(ref _subtitle, value);
    }

    /// <summary>Job Destination folder when known (Save to… / Core destination).</summary>
    public string Destination
    {
        get => _destination;
        set => SetField(ref _destination, value);
    }

    public string StatusIcon
    {
        get => _statusIcon;
        set => SetField(ref _statusIcon, value);
    }

    public string ActionLabel
    {
        get => _actionLabel;
        set => SetField(ref _actionLabel, value);
    }

    /// <summary>Hide primary action for Completed (Discard is the only row action).</summary>
    public Visibility PrimaryActionVisible
    {
        get => _primaryActionVisible;
        set => SetField(ref _primaryActionVisible, value);
    }

    public double ProgressPercent
    {
        get => _progressPercent;
        set => SetField(ref _progressPercent, value);
    }

    public DownloadStatus Status
    {
        get => _status;
        set => SetField(ref _status, value);
    }

    private void SetField<T>(ref T field, T value, [CallerMemberName] string? name = null)
    {
        if (Equals(field, value))
        {
            return;
        }
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
    }
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
        ActiveJobLimitBox.Value = 3;
    }

    /// <summary>Sync the Active Job Limit field from Core settings.</summary>
    public void SetActiveJobLimitDisplay(int limit)
    {
        ActiveJobLimitBox.Value = limit;
    }

    public void SetAudioOnlyModeDisplay(bool enabled)
    {
        AudioOnlyModeBox.IsChecked = enabled;
    }

    public void SetMediaExtrasDefaultsDisplay(MediaExtrasOptions options)
    {
        WriteSubsDefaultBox.IsChecked = options.WriteSubs;
        WriteThumbnailDefaultBox.IsChecked = options.WriteThumbnail;
    }

    private void OnMediaExtrasDefaultsApply(object sender, RoutedEventArgs e)
    {
        MediaExtrasDefaultsChanged?.Invoke(this, new MediaExtrasOptions
        {
            WriteSubs = WriteSubsDefaultBox.IsChecked == true,
            WriteThumbnail = WriteThumbnailDefaultBox.IsChecked == true,
        });
    }

    public int ActiveJobLimitValue => (int)ActiveJobLimitBox.Value;

    /// <summary>Raised when the user clicks Discard on a row (before confirmation).</summary>
    public event EventHandler<DownloadItemViewModel>? DiscardRequested;

    /// <summary>Raised when the user reorders a row (fromIndex, toIndex).</summary>
    public event EventHandler<(int FromIndex, int ToIndex)>? ReorderRequested;

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
                item.ProgressPercent = Phlegethon.ProgressDisplay.DisplayPercent(bytesDownloaded, totalBytes) ?? 0;
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

    /// <summary>Remove a job row after confirmed Discard (or Core JobDiscarded).</summary>
    public void RemoveJob(string jobId)
    {
        for (var i = 0; i < _items.Count; i++)
        {
            if (_items[i].JobId == jobId)
            {
                _items.RemoveAt(i);
                UpdateEmptyState();
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
                item.Status = DownloadStatus.Paused;
                UpdateVisualStatus(item);
                break;
            case DownloadStatus.Paused:
                item.Status = DownloadStatus.Downloading;
                UpdateVisualStatus(item);
                break;
            case DownloadStatus.Failed:
                // Retry stays local until Core pause/resume/retry wiring lands.
                item.Status = DownloadStatus.Queued;
                item.Subtitle = "Queued";
                UpdateVisualStatus(item);
                break;
            case DownloadStatus.Completed:
                // Completed uses Discard only (always-confirm flow).
                break;
        }
    }

    private void OnDiscardClick(object sender, RoutedEventArgs e)
    {
        var button = (Button)sender;
        var item = (DownloadItemViewModel)button.DataContext;
        DiscardRequested?.Invoke(this, item);
    }

    private void OnMoveUpClick(object sender, RoutedEventArgs e)
    {
        var button = (Button)sender;
        var item = (DownloadItemViewModel)button.DataContext;
        var index = _items.IndexOf(item);
        if (index > 0)
        {
            ReorderRequested?.Invoke(this, (index, index - 1));
        }
    }

    private void OnMoveDownClick(object sender, RoutedEventArgs e)
    {
        var button = (Button)sender;
        var item = (DownloadItemViewModel)button.DataContext;
        var index = _items.IndexOf(item);
        if (index >= 0 && index < _items.Count - 1)
        {
            ReorderRequested?.Invoke(this, (index, index + 1));
        }
    }

    private void OnActiveJobLimitApply(object sender, RoutedEventArgs e)
    {
        ActiveJobLimitChanged?.Invoke(this, ActiveJobLimitValue);
    }

    private void OnAudioOnlyModeChanged(object sender, RoutedEventArgs e)
    {
        AudioOnlyModeChanged?.Invoke(this, AudioOnlyModeBox.IsChecked == true);
    }

    /// <summary>Raised when the user applies a new Active Job Limit.</summary>
    public event EventHandler<int>? ActiveJobLimitChanged;

    /// <summary>Raised when the user toggles audio-only mode.</summary>
    public event EventHandler<bool>? AudioOnlyModeChanged;

    /// <summary>Raised when the user applies media extras defaults.</summary>
    public event EventHandler<MediaExtrasOptions>? MediaExtrasDefaultsChanged;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    private static string FormatProgress(ulong bytesDownloaded, ulong? totalBytes)
    {
        var percent = Phlegethon.ProgressDisplay.DisplayPercent(bytesDownloaded, totalBytes);
        if (percent.HasValue && totalBytes.HasValue)
        {
            var downloaded = FormatBytes(bytesDownloaded);
            var total = FormatBytes(totalBytes.Value);
            return $"{downloaded} / {total} ({percent.Value:F1}%)";
        }

        return $"{FormatBytes(bytesDownloaded)} downloaded";
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
                item.PrimaryActionVisible = Visibility.Visible;
                break;
            case DownloadStatus.Downloading:
                item.StatusIcon = "↓";
                item.ActionLabel = "Pause";
                item.PrimaryActionVisible = Visibility.Visible;
                break;
            case DownloadStatus.Paused:
                item.StatusIcon = "⏸";
                item.ActionLabel = "Resume";
                item.PrimaryActionVisible = Visibility.Visible;
                break;
            case DownloadStatus.Completed:
                item.StatusIcon = "✓";
                item.ActionLabel = "Discard";
                item.PrimaryActionVisible = Visibility.Collapsed;
                break;
            case DownloadStatus.Failed:
                item.StatusIcon = "✗";
                item.ActionLabel = "Retry";
                item.PrimaryActionVisible = Visibility.Visible;
                break;
        }
    }

    private void UpdateEmptyState()
    {
        EmptyStateText.Visibility = _items.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }
}
