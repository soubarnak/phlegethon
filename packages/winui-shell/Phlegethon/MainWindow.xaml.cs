using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Phlegethon.Controls;
using Phlegethon.Services;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;

namespace Phlegethon;

public sealed partial class MainWindow : Window
{
    private readonly EventChannelService _eventService;
    private readonly Microsoft.UI.Dispatching.DispatcherTimer _clipboardTimer;
    private string? _lastClipboardUrl;

    private enum DuplicateResolution
    {
        EnqueueAnyway,
        FocusExisting,
        Cancel,
    }

    public MainWindow()
    {
        this.InitializeComponent();
        _eventService = new EventChannelService();
        _eventService.OnProbeResult += OnProbeResult;
        _eventService.OnJobProgress += OnJobProgress;
        _eventService.OnJobCompleted += OnJobCompleted;
        _eventService.OnJobFailed += OnJobFailed;
        _eventService.OnJobQueued += OnJobQueued;
        _eventService.OnJobDiscarded += OnJobDiscarded;

        _clipboardTimer = new Microsoft.UI.Dispatching.DispatcherTimer
        {
            Interval = TimeSpan.FromSeconds(1.5),
        };
        _clipboardTimer.Tick += OnClipboardTimerTick;

        // Start the event loop background task
        _ = _eventService.StartEventLoopAsync();

        // Register paste/drop handlers
        this.Content.AllowDrop = true;
        this.Content.Drop += OnDrop;
        this.Content.PrePaste += OnPaste;

        DownloadQueueView.DiscardRequested += OnDiscardRequested;
        DownloadQueueView.ReorderRequested += OnReorderRequested;
        DownloadQueueView.ActiveJobLimitChanged += OnActiveJobLimitChanged;
        DownloadQueueView.AudioOnlyModeChanged += OnAudioOnlyModeChanged;

        DownloadQueueView.SetActiveJobLimitDisplay((int)_eventService.Core.GetActiveJobLimit());
        DownloadQueueView.SetAudioOnlyModeDisplay(_eventService.GetAudioOnlyMode());
        DownloadQueueView.SetMediaExtrasDefaultsDisplay(_eventService.GetDefaultMediaExtras());
        DownloadQueueView.SetCookieChannelDisplay(_eventService.GetCookieChannel());
        DownloadQueueView.SetFilenameTemplateDisplay(_eventService.GetFilenameTemplate());
        DownloadQueueView.MediaExtrasDefaultsChanged += OnMediaExtrasDefaultsChanged;
        DownloadQueueView.CookieChannelChanged += OnCookieChannelChanged;
        DownloadQueueView.FilenameTemplateChanged += OnFilenameTemplateChanged;

        // Wire up probe sheet
        ProbeSheetView.ResolveDestinationPreview = json => _eventService.PreviewProbeDestination(json);
        ProbeSheetView.ResolveDefaultQuality = qualities =>
        {
            var mapped = qualities
                .Select(q => new Quality { Id = q.Id, Label = q.Label })
                .ToList();
            return _eventService.SelectDefaultQuality(mapped);
        };
        ProbeSheetView.ResolveDefaultMediaExtras = () => _eventService.GetDefaultMediaExtras();
        ProbeSheetView.ResolveMediaExtrasCapabilities = () => _eventService.GetMediaExtrasCapabilities();
        ProbeSheetView.Canceled += (_, _) => OnProbeConfirmed();
        ProbeSheetView.Confirmed += (_, json) =>
        {
            _ = Task.Run(() =>
            {
                try
                {
                    _eventService.ConfirmProbeSelection(json);
                }
                catch (Exception ex)
                {
                    System.Diagnostics.Debug.WriteLine($"Confirm error: {ex.Message}");
                }
            });
            OnProbeConfirmed();
        };
    }

    // -----------------------------------------------------------------------
    // URL input flows (#57)
    // -----------------------------------------------------------------------

    private async void OnEnqueueClick(object sender, RoutedEventArgs e)
    {
        await ProcessUrlInputAsync(UrlInput.Text);
        UrlInput.Text = string.Empty;
    }

    private async void OnUrlInputKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key != Windows.System.VirtualKey.Enter)
        {
            return;
        }

        e.Handled = true;
        await ProcessUrlInputAsync(UrlInput.Text);
        UrlInput.Text = string.Empty;
    }

    private async Task ProcessUrlInputAsync(string text)
    {
        var urls = _eventService.SplitUrls(text);
        if (urls.Count == 0)
        {
            UrlStatusText.Text = "No valid HTTP(S) URLs found.";
            return;
        }

        var enqueued = 0;
        foreach (var url in urls)
        {
            var resolution = await ResolveDuplicateAsync(url);
            if (resolution == DuplicateResolution.Cancel)
            {
                continue;
            }

            if (resolution == DuplicateResolution.FocusExisting)
            {
                continue;
            }

            _eventService.EnqueueUrl(url);
            enqueued++;
        }

        UrlStatusText.Text = enqueued switch
        {
            0 => "No URLs enqueued.",
            1 => $"Enqueued: {urls[0]}",
            _ => $"Enqueued {enqueued} URLs.",
        };
    }

    private async Task<DuplicateResolution> ResolveDuplicateAsync(string url)
    {
        var match = _eventService.FindActiveJobForUrl(url);
        if (match == null)
        {
            return DuplicateResolution.EnqueueAnyway;
        }

        var dialog = new ContentDialog
        {
            Title = "URL already active",
            Content = new TextBlock
            {
                Text = $"This URL is already {match.Status}:\n{match.Url}",
                TextWrapping = TextWrapping.Wrap,
                FontFamily = (Windows.UI.Xaml.Media.FontFamily)Application.Current.Resources["GeistSansFont"],
            },
            PrimaryButtonText = "Enqueue anyway",
            SecondaryButtonText = "Focus existing",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Secondary,
            XamlRoot = this.Content.XamlRoot,
        };

        var result = await dialog.ShowAsync();
        return result switch
        {
            ContentDialogResult.Primary => DuplicateResolution.EnqueueAnyway,
            ContentDialogResult.Secondary => FocusExistingForMatch(match)
                ? DuplicateResolution.FocusExisting
                : DuplicateResolution.Cancel,
            _ => DuplicateResolution.Cancel,
        };
    }

    private bool FocusExistingForMatch(ActiveJobMatch match)
    {
        if (!string.IsNullOrEmpty(match.JobId))
        {
            return DownloadQueueView.FocusJob(match.JobId);
        }

        var normalized = _eventService.NormalizeUrl(match.Url);
        if (normalized == null)
        {
            return false;
        }

        var item = DownloadQueueView.FindItemByUrl(normalized, _eventService.NormalizeUrl);
        if (item == null)
        {
            UrlStatusText.Text = "URL is already being probed.";
            return true;
        }

        return DownloadQueueView.FocusJob(item.JobId);
    }

    private void OnClipboardWatcherChanged(object sender, RoutedEventArgs e)
    {
        if (ClipboardWatcherBox.IsChecked == true)
        {
            _lastClipboardUrl = null;
            _clipboardTimer.Start();
            UrlStatusText.Text = "Watching clipboard for URLs (will not auto-enqueue).";
        }
        else
        {
            _clipboardTimer.Stop();
            _lastClipboardUrl = null;
            UrlStatusText.Text = "Clipboard watcher off.";
        }
    }

    private async void OnClipboardTimerTick(object? sender, object e)
    {
        if (ClipboardWatcherBox.IsChecked != true)
        {
            return;
        }

        try
        {
            var dataPackage = Clipboard.GetContent();
            if (!dataPackage.Contains(StandardDataFormats.Text))
            {
                return;
            }

            var text = await dataPackage.GetTextAsync();
            var urls = _eventService.SplitUrls(text);
            if (urls.Count == 0)
            {
                return;
            }

            var url = urls[0];
            if (url == _lastClipboardUrl)
            {
                return;
            }

            _lastClipboardUrl = url;
            UrlInput.Text = urls.Count == 1 ? url : string.Join(Environment.NewLine, urls);
            UrlStatusText.Text = urls.Count == 1
                ? "URL detected in clipboard — review and press Enqueue."
                : $"{urls.Count} URLs detected in clipboard — review and press Enqueue.";
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"Clipboard watcher error: {ex.Message}");
        }
    }

    // -----------------------------------------------------------------------
    // Paste / Drop
    // -----------------------------------------------------------------------

    /// <summary>
    /// Handle a keyboard paste (Ctrl+V) — split URLs and enqueue.
    /// </summary>
    private async void OnPaste(object sender, RoutedEventArgs e)
    {
        var dataPackage = Clipboard.GetContent();
        if (!dataPackage.Contains(StandardDataFormats.Text))
        {
            return;
        }

        var text = await dataPackage.GetTextAsync();
        if (string.IsNullOrWhiteSpace(text))
        {
            return;
        }

        e.Handled = true;
        UrlInput.Text = text;
        await ProcessUrlInputAsync(text);
        UrlInput.Text = string.Empty;
    }

    /// <summary>
    /// Handle drag/drop of URLs or files onto the window.
    /// </summary>
    private async void OnDrop(object sender, DragEventArgs e)
    {
        e.AcceptOperation = DataPackageOperation.Link;

        if (e.DataView.Contains(StandardDataFormats.Text))
        {
            var text = await e.DataView.GetTextAsync();
            UrlInput.Text = text;
            await ProcessUrlInputAsync(text);
            UrlInput.Text = string.Empty;
            return;
        }

        if (e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            var items = await e.DataView.GetStorageItemsAsync();
            foreach (var item in items)
            {
                if (item is StorageFile file && file.FileType == ".url")
                {
                    var text = await FileIO.ReadTextAsync(file);
                    await ProcessUrlInputAsync(text);
                }
            }
        }
    }

    /// <summary>
    /// Handle "Add URL" button click — show a dialog for URL entry.
    /// </summary>
    private async void OnAddUrlClick(object sender, RoutedEventArgs e)
    {
        var dialog = new ContentDialog
        {
            Title = "Add Download URL(s)",
            Content = new TextBox
            {
                PlaceholderText = "https://example.com/video.mp4",
                AcceptsReturn = true,
                TextWrapping = TextWrapping.Wrap,
                MinHeight = 96,
                FontFamily = (Windows.UI.Xaml.Media.FontFamily)Application.Current.Resources["GeistMonoFont"],
            },
            PrimaryButtonText = "Add",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = this.Content.XamlRoot,
        };

        var result = await dialog.ShowAsync();
        if (result != ContentDialogResult.Primary)
        {
            return;
        }

        var textBox = (TextBox)dialog.Content;
        var text = textBox.Text.Trim();
        if (!string.IsNullOrEmpty(text))
        {
            await ProcessUrlInputAsync(text);
        }
    }

    // -----------------------------------------------------------------------
    // Event handlers
    // -----------------------------------------------------------------------

    private void OnProbeResult(object? sender, ProbeResultEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            ProbeSheetView.ShowProbeResult(args.Url, args.Items, args.SourceName);
            DownloadQueueView.Visibility = Visibility.Collapsed;
            ProbeSheetView.Visibility = Visibility.Visible;
        });
    }

    private void OnJobQueued(object? sender, JobQueuedEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            var subtitle = string.IsNullOrEmpty(args.Destination)
                ? "Queued · Library Root default"
                : $"Save to {args.Destination}";
            DownloadQueueView.AddOrUpdateJob(
                args.JobId,
                args.Title,
                subtitle,
                DownloadStatus.Queued,
                0,
                string.IsNullOrEmpty(args.Destination) ? null : args.Destination,
                args.Url);
        });
    }

    private void OnJobProgress(object? sender, JobProgressEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            DownloadQueueView.UpdateJobProgress(args.JobId, args.BytesDownloaded, args.TotalBytes);
        });
    }

    private void OnJobCompleted(object? sender, JobCompletedEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            DownloadQueueView.MarkJobCompleted(args.JobId, args.Path);
        });
    }

    private void OnJobFailed(object? sender, JobFailedEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            DownloadQueueView.MarkJobFailed(args.JobId, args.Error);
        });
    }

    private void OnJobDiscarded(object? sender, JobDiscardedEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            DownloadQueueView.RemoveJob(args.JobId);
        });
    }

    private void OnReorderRequested(object? sender, (int FromIndex, int ToIndex) args)
    {
        try
        {
            _eventService.ReorderJob(args.FromIndex, args.ToIndex);
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Reorder failed: {ex.Message}";
        }
    }

    private void OnActiveJobLimitChanged(object? sender, int limit)
    {
        try
        {
            _eventService.SetActiveJobLimit((uint)limit);
            UrlStatusText.Text = $"Active Job Limit set to {limit}";
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Active Job Limit failed: {ex.Message}";
        }
    }

    private void OnAudioOnlyModeChanged(object? sender, bool enabled)
    {
        try
        {
            _eventService.SetAudioOnlyMode(enabled);
            UrlStatusText.Text = enabled
                ? "Audio-only mode enabled"
                : "Audio-only mode disabled";
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Audio-only mode failed: {ex.Message}";
        }
    }

    private void OnMediaExtrasDefaultsChanged(object? sender, MediaExtrasOptions options)
    {
        try
        {
            _eventService.SetDefaultMediaExtras(options);
            UrlStatusText.Text = "Media extras defaults updated";
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Media extras defaults failed: {ex.Message}";
        }
    }

    private void OnCookieChannelChanged(object? sender, CookieChannel channel)
    {
        try
        {
            _eventService.SetCookieChannel(channel);
            UrlStatusText.Text = "Cookie auth channel updated";
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Cookie channel failed: {ex.Message}";
        }
    }

    private void OnFilenameTemplateChanged(object? sender, string template)
    {
        try
        {
            _eventService.SetFilenameTemplate(template);
            UrlStatusText.Text = "Filename template updated";
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Filename template failed: {ex.Message}";
        }
    }

    private async void OnDiscardRequested(object? sender, DownloadItemViewModel item)
    {
        var copy = DiscardConfirmCopy.Build(item.Status, item.Destination);
        var dialog = new ContentDialog
        {
            Title = copy.Title,
            Content = new TextBlock
            {
                Text = copy.Body,
                TextWrapping = TextWrapping.Wrap,
                FontFamily = (Windows.UI.Xaml.Media.FontFamily)Application.Current.Resources["GeistSansFont"],
            },
            PrimaryButtonText = copy.ConfirmLabel,
            CloseButtonText = copy.CancelLabel,
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = this.Content.XamlRoot,
        };

        var result = await dialog.ShowAsync();
        if (result != ContentDialogResult.Primary)
        {
            return;
        }

        try
        {
            await Task.Run(() => _eventService.DiscardJob(item.JobId));
            DownloadQueueView.RemoveJob(item.JobId);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"Discard error: {ex.Message}");
            UrlStatusText.Text = $"Discard failed: {ex.Message}";
        }
    }

    /// <summary>
    /// Called by ProbeSheetControl when the user confirms a selection.
    /// Switches back to the download queue view.
    /// </summary>
    public void OnProbeConfirmed()
    {
        DownloadQueueView.Visibility = Visibility.Visible;
        ProbeSheetView.Visibility = Visibility.Collapsed;
    }
}
