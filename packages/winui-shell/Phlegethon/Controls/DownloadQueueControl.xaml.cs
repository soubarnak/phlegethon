using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using Phlegethon;
using Phlegethon.Services;
using Phlegethon.Strings;
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
    private string _url = "";
    private string _subtitle = "Queued";
    private string _destination = "";
    private string _outputPath = "";
    private string _statusIcon = "⋯";
    private string _statusAccessibleName = "Queued";
    private string _actionLabel = "Pause";
    private Visibility _primaryActionVisible = Visibility.Visible;
    private double _progressPercent;
    private ulong? _totalBytes;
    private DownloadStatus _status = DownloadStatus.Queued;
    private Visibility _progressBarVisibility = Visibility.Collapsed;
    private bool _progressIsIndeterminate;

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

    public string Url
    {
        get => _url;
        set => SetField(ref _url, value);
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

    public string StatusAccessibleName
    {
        get => _statusAccessibleName;
        set => SetField(ref _statusAccessibleName, value);
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

    /// <summary>Absolute path to the completed output file, when known.</summary>
    public string OutputPath
    {
        get => _outputPath;
        set
        {
            if (SetField(ref _outputPath, value))
            {
                CompletedActionsVisible = _status == DownloadStatus.Completed
                    && !string.IsNullOrWhiteSpace(_outputPath)
                        ? Visibility.Visible
                        : Visibility.Collapsed;
            }
        }
    }

    private Visibility _completedActionsVisible = Visibility.Collapsed;

    /// <summary>Reveal / Open actions for completed outputs.</summary>
    public Visibility CompletedActionsVisible
    {
        get => _completedActionsVisible;
        set => SetField(ref _completedActionsVisible, value);
    }

    public double ProgressPercent
    {
        get => _progressPercent;
        set
        {
            if (SetField(ref _progressPercent, value))
            {
                UpdateProgressPresentation();
            }
        }
    }

    /// <summary>Known total size when Core reports it (sort by size).</summary>
    public ulong? TotalBytes
    {
        get => _totalBytes;
        set
        {
            if (SetField(ref _totalBytes, value))
            {
                UpdateProgressPresentation();
            }
        }
    }

    public Visibility ProgressBarVisibility
    {
        get => _progressBarVisibility;
        private set => SetField(ref _progressBarVisibility, value);
    }

    public bool ProgressIsIndeterminate
    {
        get => _progressIsIndeterminate;
        private set => SetField(ref _progressIsIndeterminate, value);
    }

    public DownloadStatus Status
    {
        get => _status;
        set
        {
            if (SetField(ref _status, value))
            {
                UpdateProgressPresentation();
            }
        }
    }

    private void UpdateProgressPresentation()
    {
        var show = _status is DownloadStatus.Queued
            or DownloadStatus.Downloading
            or DownloadStatus.Completed
            || (_status == DownloadStatus.Paused && (_progressPercent > 0 || (_totalBytes ?? 0) > 0));
        ProgressBarVisibility = show ? Visibility.Visible : Visibility.Collapsed;
        ProgressIsIndeterminate = _status is DownloadStatus.Queued or DownloadStatus.Downloading
            && (!_totalBytes.HasValue || _totalBytes.Value == 0);
    }

    private bool SetField<T>(ref T field, T value, [CallerMemberName] string? name = null)
    {
        if (Equals(field, value))
        {
            return false;
        }
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
        return true;
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
    private readonly List<DownloadItemViewModel> _items = new();
    private readonly ObservableCollection<DownloadItemViewModel> _viewItems = new();

    public DownloadQueueControl()
    {
        this.InitializeComponent();
        ApplyLocalizedStrings();
        if (DownloadList != null)
        {
            DownloadList.ItemsSource = _viewItems;
        }
        ActiveJobLimitBox.Value = 3;
        RefreshView();
    }

    private void ApplyLocalizedStrings()
    {
        QueueHeaderText.Text = ShellStrings.Get("queue.title");
        Resources["QueueActionMoveUp"] = ShellStrings.Get("queue.action.moveUp");
        Resources["QueueActionMoveDown"] = ShellStrings.Get("queue.action.moveDown");
        Resources["QueueActionDiscard"] = ShellStrings.Get("queue.action.discard");
        Resources["QueueActionReveal"] = ShellStrings.Get("queue.action.reveal");
        Resources["QueueActionOpen"] = ShellStrings.Get("queue.action.open");

        QueueSearchBox.PlaceholderText = ShellStrings.Get("queue.search.placeholder");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            QueueSearchBox, ShellStrings.Get("queue.search.label"));

        var filterItems = new (string key, string tag)[]
        {
            ("queue.filter.all", "all"),
            ("queue.filter.queued", "queued"),
            ("queue.filter.downloading", "downloading"),
            ("queue.filter.paused", "paused"),
            ("queue.filter.completed", "completed"),
            ("queue.filter.failed", "failed"),
        };
        QueueFilterBox.Items.Clear();
        foreach (var (key, tag) in filterItems)
        {
            QueueFilterBox.Items.Add(new ComboBoxItem
            {
                Content = ShellStrings.Get(key),
                Tag = tag,
            });
        }
        QueueFilterBox.SelectedIndex = 0;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            QueueFilterBox, ShellStrings.Get("queue.filter.label"));

        var sortItems = new (string key, string tag)[]
        {
            ("queue.sort.queue", "queue"),
            ("queue.sort.title", "title"),
            ("queue.sort.status", "status"),
            ("queue.sort.size", "size"),
        };
        QueueSortBox.Items.Clear();
        foreach (var (key, tag) in sortItems)
        {
            QueueSortBox.Items.Add(new ComboBoxItem
            {
                Content = ShellStrings.Get(key),
                Tag = tag,
            });
        }
        QueueSortBox.SelectedIndex = 0;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            QueueSortBox, ShellStrings.Get("queue.sort.label"));

        StatusFooter.Text = ShellStrings.Get("queue.footer.ready");
        ActiveLimitLabel.Text = ShellStrings.Get("queue.footer.activeLimit");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetLabeledBy(
            ActiveJobLimitBox, ActiveLimitLabel);
    }

    /// <summary>Update the footer status text (probe errors, ready, etc.).</summary>
    public void SetStatus(string message)
    {
        StatusFooter.Text = message;
    }

    /// <summary>Sync the Active Job Limit field from Core settings.</summary>
    public void SetActiveJobLimitDisplay(int limit)
    {
        ActiveJobLimitBox.Value = limit;
    }

    public int ActiveJobLimitValue => (int)ActiveJobLimitBox.Value;

    /// <summary>Raised when the user clicks Discard on a row (before confirmation).</summary>
    public event EventHandler<DownloadItemViewModel>? DiscardRequested;

    /// <summary>Raised when the user clicks Reveal on a completed row.</summary>
    public event EventHandler<DownloadItemViewModel>? RevealRequested;

    /// <summary>Raised when the user clicks Open on a completed row.</summary>
    public event EventHandler<DownloadItemViewModel>? OpenRequested;

    /// <summary>Raised when the user reorders a row (fromIndex, toIndex).</summary>
    public event EventHandler<(int FromIndex, int ToIndex)>? ReorderRequested;

    public event EventHandler<DownloadItemViewModel>? PauseRequested;
    public event EventHandler<DownloadItemViewModel>? ResumeRequested;
    public event EventHandler<DownloadItemViewModel>? RetryRequested;

    /// <summary>
    /// Add or update a job row.
    /// </summary>
    public void AddOrUpdateJob(string jobId, string title, string subtitle, DownloadStatus status, double progress, string? destination = null, string? url = null)
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
                if (url != null)
                {
                    item.Url = url;
                }
                UpdateVisualStatus(item);
                RefreshView();
                return;
            }
        }

        // Add new
        var newItem = new DownloadItemViewModel
        {
            JobId = jobId,
            Title = title,
            Url = url ?? "",
            Subtitle = subtitle,
            Destination = destination ?? "",
            Status = status,
            ProgressPercent = progress,
        };
        UpdateVisualStatus(newItem);
        _items.Add(newItem);

        RefreshView();
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
                item.TotalBytes = totalBytes;
                var progressText = FormatProgress(bytesDownloaded, totalBytes);
                item.Subtitle = string.IsNullOrEmpty(item.Destination)
                    ? progressText
                    : $"{progressText} · {item.Destination}";
                item.ProgressPercent = Phlegethon.ProgressDisplay.DisplayPercent(bytesDownloaded, totalBytes) ?? 0;
                UpdateVisualStatus(item);
                RefreshView();
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
                item.OutputPath = path;
                var dest = string.IsNullOrEmpty(item.Destination)
                    ? (System.IO.Path.GetDirectoryName(path) ?? "")
                    : item.Destination;
                item.Subtitle = string.IsNullOrEmpty(dest)
                    ? $"Downloaded to {path}"
                    : $"Downloaded to {path} · Save to {dest}";
                item.ProgressPercent = 100;
                UpdateVisualStatus(item);
                RefreshView();
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
        foreach (var item in _items)
        {
            if (item.JobId == jobId)
            {
                item.OutputPath = path;
                break;
            }
        }
        RefreshView();
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
                RefreshView();
                return;
            }
        }

        AddOrUpdateJob(
            jobId,
            "Download",
            $"Failed: {error}",
            DownloadStatus.Failed,
            0);
    }

    public void MarkJobPaused(string jobId)
    {
        foreach (var item in _items)
        {
            if (item.JobId == jobId)
            {
                item.Status = DownloadStatus.Paused;
                UpdateVisualStatus(item);
                RefreshView();
                return;
            }
        }
    }

    public void MarkJobResumed(string jobId)
    {
        foreach (var item in _items)
        {
            if (item.JobId == jobId)
            {
                item.Status = DownloadStatus.Queued;
                item.Subtitle = "Queued";
                UpdateVisualStatus(item);
                RefreshView();
                return;
            }
        }
    }

    public void MarkJobRetried(string jobId)
    {
        foreach (var item in _items)
        {
            if (item.JobId == jobId)
            {
                item.Status = DownloadStatus.Queued;
                item.Subtitle = "Queued";
                UpdateVisualStatus(item);
                RefreshView();
                return;
            }
        }
    }

    public void HydrateFromSnapshots(IReadOnlyList<JobSnapshot> snapshots)
    {
        _items.Clear();
        foreach (var snap in snapshots)
        {
            var status = MapSnapshotStatus(snap.Status);
            var progress = status == DownloadStatus.Completed
                ? 100.0
                : Phlegethon.ProgressDisplay.DisplayPercent(snap.BytesDownloaded, snap.TotalBytes) ?? 0;
            var subtitle = status switch
            {
                DownloadStatus.Failed => "Failed",
                DownloadStatus.Paused => "Paused",
                DownloadStatus.Completed => $"Downloaded to {snap.Destination}",
                DownloadStatus.Downloading => FormatProgress(snap.BytesDownloaded, snap.TotalBytes),
                _ => string.IsNullOrEmpty(snap.Destination) ? "Queued" : $"Save to {snap.Destination}",
            };
            var item = new DownloadItemViewModel
            {
                JobId = snap.JobId,
                Title = snap.Title,
                Url = snap.Url,
                Destination = snap.Destination,
                Subtitle = subtitle,
                Status = status,
                ProgressPercent = progress,
                TotalBytes = snap.TotalBytes,
            };
            UpdateVisualStatus(item);
            _items.Add(item);
        }

        RefreshView();
    }

    private static DownloadStatus MapSnapshotStatus(string status) => status switch
    {
        "paused" => DownloadStatus.Paused,
        "failed" => DownloadStatus.Failed,
        "completed" => DownloadStatus.Completed,
        "pending" => DownloadStatus.Queued,
        _ => DownloadStatus.Queued,
    };

    /// <summary>Scroll to and highlight a queue row by job id.</summary>
    public bool FocusJob(string jobId)
    {
        if (DownloadList == null)
        {
            return false;
        }

        for (var i = 0; i < _viewItems.Count; i++)
        {
            if (_viewItems[i].JobId != jobId)
            {
                continue;
            }

            DownloadList.SelectedIndex = i;
            DownloadList.ScrollIntoView(_viewItems[i]);
            if (DownloadList.ContainerFromIndex(i) is ListViewItem container)
            {
                container.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            }
            return true;
        }

        return false;
    }

    /// <summary>Find a row title by job id.</summary>
    public string? FindJobTitle(string jobId)
    {
        foreach (var item in _items)
        {
            if (item.JobId == jobId)
            {
                return item.Title;
            }
        }

        return null;
    }

    /// <summary>Find a row by normalized URL match.</summary>
    public DownloadItemViewModel? FindItemByUrl(string normalizedUrl, Func<string, string?> normalize)
    {
        foreach (var item in _items)
        {
            if (string.IsNullOrEmpty(item.Url))
            {
                continue;
            }

            var itemNormalized = normalize(item.Url);
            if (itemNormalized == normalizedUrl)
            {
                return item;
            }
        }

        return null;
    }

    /// <summary>Remove a job row after confirmed Discard (or Core JobDiscarded).</summary>
    public void RemoveJob(string jobId)
    {
        for (var i = 0; i < _items.Count; i++)
        {
            if (_items[i].JobId == jobId)
            {
                _items.RemoveAt(i);
                RefreshView();
                return;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Event handlers
    // -----------------------------------------------------------------------

    private void OnQueueSearchChanged(object sender, TextChangedEventArgs e) => RefreshView();

    private void OnQueueFilterChanged(object sender, SelectionChangedEventArgs e) => RefreshView();

    private void OnQueueSortChanged(object sender, SelectionChangedEventArgs e) => RefreshView();

    private void OnItemAction(object sender, RoutedEventArgs e)
    {
        var button = (Button)sender;
        var item = (DownloadItemViewModel)button.DataContext;

        switch (item.Status)
        {
            case DownloadStatus.Downloading:
            case DownloadStatus.Queued:
                PauseRequested?.Invoke(this, item);
                break;
            case DownloadStatus.Paused:
                ResumeRequested?.Invoke(this, item);
                break;
            case DownloadStatus.Failed:
                RetryRequested?.Invoke(this, item);
                break;
            case DownloadStatus.Completed:
                break;
        }
    }

    private void OnDiscardClick(object sender, RoutedEventArgs e)
    {
        var button = (Button)sender;
        var item = (DownloadItemViewModel)button.DataContext;
        DiscardRequested?.Invoke(this, item);
    }

    private void OnRevealClick(object sender, RoutedEventArgs e)
    {
        var button = (Button)sender;
        var item = (DownloadItemViewModel)button.DataContext;
        if (!string.IsNullOrWhiteSpace(item.OutputPath))
        {
            RevealRequested?.Invoke(this, item);
        }
    }

    private void OnOpenClick(object sender, RoutedEventArgs e)
    {
        var button = (Button)sender;
        var item = (DownloadItemViewModel)button.DataContext;
        if (!string.IsNullOrWhiteSpace(item.OutputPath))
        {
            OpenRequested?.Invoke(this, item);
        }
    }

    private void OnMoveUpClick(object sender, RoutedEventArgs e)
    {
        var button = (Button)sender;
        var item = (DownloadItemViewModel)button.DataContext;
        var index = _items.IndexOf(item);
        if (index > 0)
        {
            _items.RemoveAt(index);
            _items.Insert(index - 1, item);
            RefreshView();
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
            _items.RemoveAt(index);
            _items.Insert(index + 1, item);
            RefreshView();
            ReorderRequested?.Invoke(this, (index, index + 1));
        }
    }

    private void OnActiveJobLimitApply(object sender, RoutedEventArgs e)
    {
        ActiveJobLimitChanged?.Invoke(this, ActiveJobLimitValue);
    }

    /// <summary>Raised when the user applies a new Active Job Limit.</summary>
    public event EventHandler<int>? ActiveJobLimitChanged;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    private string CurrentSearch => QueueSearchBox?.Text ?? "";

    private string CurrentFilter =>
        (QueueFilterBox?.SelectedItem as ComboBoxItem)?.Tag as string ?? QueueView.FilterAll;

    private string CurrentSort =>
        (QueueSortBox?.SelectedItem as ComboBoxItem)?.Tag as string ?? QueueView.SortQueue;

    private void RefreshView()
    {
        var view = QueueView.Apply(_items, CurrentSearch, CurrentFilter, CurrentSort);
        _viewItems.Clear();
        foreach (var item in view)
        {
            _viewItems.Add(item);
        }

        UpdateEmptyState();
    }

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
                item.StatusAccessibleName = ShellStrings.Get("queue.status.queued");
                item.ActionLabel = ShellStrings.Get("queue.action.pause");
                item.PrimaryActionVisible = Visibility.Visible;
                break;
            case DownloadStatus.Downloading:
                item.StatusIcon = "↓";
                item.StatusAccessibleName = ShellStrings.Get("queue.status.downloading");
                item.ActionLabel = ShellStrings.Get("queue.action.pause");
                item.PrimaryActionVisible = Visibility.Visible;
                break;
            case DownloadStatus.Paused:
                item.StatusIcon = "⏸";
                item.StatusAccessibleName = ShellStrings.Get("queue.status.paused");
                item.ActionLabel = ShellStrings.Get("queue.action.resume");
                item.PrimaryActionVisible = Visibility.Visible;
                break;
            case DownloadStatus.Completed:
                item.StatusIcon = "✓";
                item.StatusAccessibleName = ShellStrings.Get("queue.status.completed");
                item.ActionLabel = ShellStrings.Get("queue.action.discard");
                item.PrimaryActionVisible = Visibility.Collapsed;
                item.CompletedActionsVisible = !string.IsNullOrWhiteSpace(item.OutputPath)
                    ? Visibility.Visible
                    : Visibility.Collapsed;
                break;
            case DownloadStatus.Failed:
                item.StatusIcon = "✗";
                item.StatusAccessibleName = ShellStrings.Get("queue.status.failed");
                item.ActionLabel = ShellStrings.Get("queue.action.retry");
                item.PrimaryActionVisible = Visibility.Visible;
                break;
        }
    }

    private void UpdateEmptyState()
    {
        if (EmptyStateText == null)
        {
            return;
        }

        if (_items.Count == 0)
        {
            EmptyStateText.Text = ShellStrings.Get("queue.empty");
            EmptyStateText.Visibility = Visibility.Visible;
        }
        else if (_viewItems.Count == 0)
        {
            EmptyStateText.Text = ShellStrings.Get("queue.emptyFiltered");
            EmptyStateText.Visibility = Visibility.Visible;
        }
        else
        {
            EmptyStateText.Visibility = Visibility.Collapsed;
        }
    }
}
