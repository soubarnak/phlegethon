using System;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Phlegethon.Controls;
using Phlegethon.Services;
using Phlegethon.Strings;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;

namespace Phlegethon;

public sealed partial class MainWindow : Window
{
    private const string DesktopNotificationsSettingKey = "DesktopNotificationsEnabled";

    private readonly EventChannelService _eventService;
    private readonly CompanionReceiverService _companionReceiver;
    private readonly DesktopNotificationService _notificationService;
    private readonly Microsoft.UI.Xaml.DispatcherTimer _clipboardTimer;
    private string? _lastClipboardUrl;
    private bool _windowActive;
    private bool _desktopNotificationsEnabled = true;
    private SettingsDialog? _openSettingsPanel;
    private string? _urlSaveToDestination;

    private enum DuplicateResolution
    {
        EnqueueAnyway,
        FocusExisting,
        Cancel,
    }

    public MainWindow()
    {
        this.InitializeComponent();
        AppWindow.Resize(new Windows.Graphics.SizeInt32(960, 720));
        _eventService = new EventChannelService();
        _companionReceiver = new CompanionReceiverService(_eventService);
        _notificationService = new DesktopNotificationService(
            DispatcherQueue,
            jobId => DownloadQueueView.FocusJob(jobId));
        _notificationService.EnsureRegistered();
        _eventService.OnProbeResult += OnProbeResult;
        _eventService.OnProbeFailed += OnProbeFailed;
        _eventService.OnJobProgress += OnJobProgress;
        _eventService.OnJobCompleted += OnJobCompleted;
        _eventService.OnJobFailed += OnJobFailed;
        _eventService.OnJobQueued += OnJobQueued;
        _eventService.OnJobDiscarded += OnJobDiscarded;
        _eventService.OnJobPaused += OnJobPaused;
        _eventService.OnJobResumed += OnJobResumed;
        _eventService.OnJobRetried += OnJobRetried;
        _eventService.OnProcessPendingError += OnProcessPendingError;
        _companionReceiver.OnCompanionIntent += OnCompanionIntent;

        SaveToButton.Content = ShellStrings.Get("url.saveTo");

        _clipboardTimer = new Microsoft.UI.Xaml.DispatcherTimer
        {
            Interval = TimeSpan.FromSeconds(1.5),
        };
        _clipboardTimer.Tick += OnClipboardTimerTick;

        // Start the event loop background task
        _ = InitializeAsync();
        _companionReceiver.Start();

        // Register drop handler (global paste uses UrlInput / Add URL dialog).
        this.Content.AllowDrop = true;
        this.Content.Drop += OnDrop;

        DownloadQueueView.DiscardRequested += OnDiscardRequested;
        DownloadQueueView.RevealRequested += OnRevealRequested;
        DownloadQueueView.OpenRequested += OnOpenRequested;
        DownloadQueueView.ReorderRequested += OnReorderRequested;
        DownloadQueueView.ActiveJobLimitChanged += OnActiveJobLimitChanged;
        DownloadQueueView.PauseRequested += OnPauseRequested;
        DownloadQueueView.ResumeRequested += OnResumeRequested;
        DownloadQueueView.RetryRequested += OnRetryRequested;

        DownloadQueueView.SetActiveJobLimitDisplay((int)_eventService.Core.GetActiveJobLimit());

        HistoryView.FilterChanged += (_, _) => RefreshHistory();
        HistoryView.RevealRequested += OnHistoryRevealRequested;
        HistoryView.OpenRequested += OnHistoryOpenRequested;
        HistoryView.ReDownloadRequested += OnHistoryReDownloadRequested;
        HistoryView.RemoveRequested += OnHistoryRemoveRequested;

        Activated += OnWindowActivated;
        Closed += (_, _) => Activated -= OnWindowActivated;

        LoadDesktopNotificationsSetting();
        DesktopNotificationsBox.IsChecked = _desktopNotificationsEnabled;

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
        ProbeSheetView.ResolveItemQualitiesAsync = (probeUrl, itemId) =>
            Task.Run(() => _eventService.EnrichProbeItemQualities(probeUrl, itemId));
        ProbeSheetView.Canceled += OnProbeCanceled;
        ProbeSheetView.Confirmed += OnProbeSheetConfirmed;
        ProbeSheetView.SaveToFailed += OnProbeSaveToFailed;
    }

    private async Task InitializeAsync()
    {
        await _eventService.StartEventLoopAsync();
        try
        {
            var snapshots = await Task.Run(() => _eventService.ListJobSnapshots());
            if (snapshots.Count > 0)
            {
                _ = DispatcherQueue.TryEnqueue(() => DownloadQueueView.HydrateFromSnapshots(snapshots));
            }
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Failed to restore queue: {ex.Message}";
        }
    }

    private void OnProbeCanceled(object? sender, EventArgs e)
    {
        try
        {
            var probeUrl = ProbeSheetView.ProbeUrl;
            if (!string.IsNullOrEmpty(probeUrl))
            {
                _eventService.ClearPendingProbe(probeUrl);
            }
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Probe cancel failed: {ex.Message}";
        }

        OnProbeConfirmed();
    }

    private void OnProbeSaveToFailed(object? sender, string error)
    {
        UrlStatusText.Text = $"Save to… failed: {error}";
    }

    private async void OnProbeSheetConfirmed(object? sender, string json)
    {
        try
        {
            var notifications = await Task.Run(() => _eventService.ConfirmProbeSelection(json));
            foreach (var args in notifications)
            {
                OnJobQueued(this, args);
            }

            if (notifications.Count == 0)
            {
                throw new InvalidOperationException("Confirm succeeded but no jobs were created.");
            }

            OnProbeConfirmed();
            UrlStatusText.Text = $"Downloads queued: {notifications.Count} job(s).";
        }
        catch (Exception ex)
        {
            var dialog = new ContentDialog
            {
                Title = "Could not queue downloads",
                Content = new TextBlock
                {
                    Text = ex.Message,
                    TextWrapping = TextWrapping.WrapWholeWords,
                    FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
                    MaxWidth = 520,
                },
                CloseButtonText = "OK",
                DefaultButton = ContentDialogButton.Close,
                XamlRoot = Content.XamlRoot,
            };
            _ = await dialog.ShowAsync();
        }
    }

    private bool _historyTabActive;

    private void OnQueueTabClick(object sender, RoutedEventArgs e) => ShowQueueTab();

    private void OnHistoryTabClick(object sender, RoutedEventArgs e) => ShowHistoryTab();

    private void ShowQueueTab()
    {
        _historyTabActive = false;
        DownloadQueueView.Visibility = Visibility.Visible;
        HistoryView.Visibility = Visibility.Collapsed;
        ProbeSheetView.Visibility = Visibility.Collapsed;
    }

    private void ShowHistoryTab()
    {
        _historyTabActive = true;
        DownloadQueueView.Visibility = Visibility.Collapsed;
        HistoryView.Visibility = Visibility.Visible;
        ProbeSheetView.Visibility = Visibility.Collapsed;
        RefreshHistory();
    }

    private void RefreshHistory()
    {
        try
        {
            var filter = HistoryView.CurrentFilter();
            var entries = filter == "all"
                ? _eventService.ListHistory()
                : _eventService.ListHistoryByOutcome(filter);
            HistoryView.SetEntries(entries, filter);
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Failed to load history: {ex.Message}";
        }
    }

    private void OnHistoryRevealRequested(object? sender, Controls.HistoryItemViewModel item)
    {
        try
        {
            OutputFileActions.RevealInFolder(item.Destination);
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Reveal failed: {ex.Message}";
        }
    }

    private void OnHistoryOpenRequested(object? sender, Controls.HistoryItemViewModel item)
    {
        if (string.Equals(item.Outcome, "Failed", StringComparison.OrdinalIgnoreCase))
        {
            UrlStatusText.Text = "Open is only available for completed downloads.";
            return;
        }

        try
        {
            OutputFileActions.OpenFile(item.Destination);
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Open failed: {ex.Message}";
        }
    }

    private async void OnHistoryReDownloadRequested(object? sender, Controls.HistoryItemViewModel item)
    {
        ShowQueueTab();
        UrlInput.Text = item.Url;
        await ProcessUrlInputAsync(item.Url);
        UrlInput.Text = string.Empty;
    }

    private void OnHistoryRemoveRequested(object? sender, Controls.HistoryItemViewModel item)
    {
        try
        {
            _eventService.RemoveHistoryEntry(item.MediaId);
            RefreshHistory();
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Remove from History failed: {ex.Message}";
        }
    }

    private void MaybeRefreshHistory()
    {
        if (_historyTabActive)
        {
            RefreshHistory();
        }
    }

    // -----------------------------------------------------------------------
    // URL input flows (#57)
    // -----------------------------------------------------------------------

    private async void OnEnqueueClick(object sender, RoutedEventArgs e)
    {
        try
        {
            await ProcessUrlInputAsync(UrlInput.Text);
            UrlInput.Text = string.Empty;
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Enqueue failed: {ex.Message}";
        }
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

            _eventService.EnqueueUrl(url, _urlSaveToDestination);
            enqueued++;
        }

        UpdateUrlSaveToHint();

        UrlStatusText.Text = enqueued switch
        {
            0 => "No URLs enqueued.",
            1 => $"Processing: {urls[0]}",
            _ => $"Processing {enqueued} URLs.",
        };
    }

    private void OnSaveToClick(object sender, RoutedEventArgs e)
    {
        try
        {
            var path = FolderPickerService.PickFolder(this, ShellStrings.Get("url.saveTo"));
            if (!string.IsNullOrEmpty(path))
            {
                _urlSaveToDestination = path;
                UpdateUrlSaveToHint();
            }
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Save to… failed: {ex.Message}";
        }
    }

    private void UpdateUrlSaveToHint()
    {
        if (string.IsNullOrEmpty(_urlSaveToDestination))
        {
            SaveToHintText.Visibility = Visibility.Collapsed;
            SaveToHintText.Text = "";
            return;
        }

        SaveToHintText.Visibility = Visibility.Visible;
        SaveToHintText.Text = $"Save to: {_urlSaveToDestination}";
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
                FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
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

    private void OnWindowActivated(object sender, WindowActivatedEventArgs args)
    {
        _windowActive = args.WindowActivationState != WindowActivationState.Deactivated;
    }

    private void LoadDesktopNotificationsSetting()
    {
        // Unpackaged shell has no package identity — Windows.Storage.ApplicationData.Current throws.
        try
        {
            var path = DesktopNotificationsSettingsPath();
            if (File.Exists(path)
                && bool.TryParse(File.ReadAllText(path).Trim(), out var enabled))
            {
                _desktopNotificationsEnabled = enabled;
            }
        }
        catch
        {
            // Keep default (_desktopNotificationsEnabled = true).
        }
    }

    private void SaveDesktopNotificationsSetting(bool enabled)
    {
        _desktopNotificationsEnabled = enabled;
        try
        {
            var path = DesktopNotificationsSettingsPath();
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllText(path, enabled ? "true" : "false");
        }
        catch
        {
            // Best-effort persistence for unpackaged local settings.
        }
    }

    private static string DesktopNotificationsSettingsPath()
    {
        var appData = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        return Path.Combine(appData, "Phlegethon", $"{DesktopNotificationsSettingKey}.txt");
    }

    private void OnDesktopNotificationsChanged(object sender, RoutedEventArgs e)
    {
        SaveDesktopNotificationsSetting(DesktopNotificationsBox.IsChecked == true);
    }

    private void MaybeNotifyJob(string jobId, string title, string headline, string detail)
    {
        if (!_desktopNotificationsEnabled || _windowActive)
        {
            return;
        }

        _notificationService.ShowJobNotification(jobId, headline, $"{title}: {detail}");
    }

    private string? FindJobTitle(string jobId) =>
        DownloadQueueView.FindJobTitle(jobId);

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
        try
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

            UrlInput.Text = text;
            await ProcessUrlInputAsync(text);
            UrlInput.Text = string.Empty;
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Paste failed: {ex.Message}";
        }
    }

    /// <summary>
    /// Handle drag/drop of URLs or files onto the window.
    /// </summary>
    private async void OnDrop(object sender, DragEventArgs e)
    {
        try
        {
            e.AcceptedOperation = DataPackageOperation.Link;

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
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Drop failed: {ex.Message}";
        }
    }

    /// <summary>
    /// Handle "Add URL" button click — show a dialog for URL entry.
    /// </summary>
    private async void OnAddUrlClick(object sender, RoutedEventArgs e)
    {
        try
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
                    FontFamily = (FontFamily)Application.Current.Resources["GeistMonoFont"],
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
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Add URL failed: {ex.Message}";
        }
    }

    // -----------------------------------------------------------------------
    // Event handlers
    // -----------------------------------------------------------------------

    private void OnCompanionIntent(object? sender, CompanionIntentEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            Activate();
            ShowQueueTab();
            UrlStatusText.Text = $"Companion hand-off: {args.Url}";
        });
    }

    private void OnProbeResult(object? sender, ProbeResultEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            if (args.Items.Count == 0)
            {
                UrlStatusText.Text = $"Probe returned no videos: {args.Url}";
                DownloadQueueView.SetStatus($"Probe empty: {args.Url}");
                return;
            }

            ProbeSheetView.ShowProbeResult(args.Url, args.Items, args.SourceName);
            DownloadQueueView.Visibility = Visibility.Collapsed;
            HistoryView.Visibility = Visibility.Collapsed;
            ProbeSheetView.Visibility = Visibility.Visible;
            ProbeSheetView.FocusInitial();
            UrlStatusText.Text = $"Probe ready: {args.Items.Count} item(s)";
        });
    }

    private void OnProbeFailed(object? sender, ProbeFailedEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(async () =>
        {
            var full = $"Probe failed ({args.Url}): {args.Error}";
            var message = TruncateStatus(full);
            UrlStatusText.Text = message;
            DownloadQueueView.SetStatus(message);

            // Dense status line truncates; modal makes network/extractor failures obvious.
            var dialog = new ContentDialog
            {
                Title = "Probe failed",
                Content = new TextBlock
                {
                    Text = full,
                    TextWrapping = TextWrapping.WrapWholeWords,
                    FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
                    MaxWidth = 520,
                },
                CloseButtonText = "OK",
                DefaultButton = ContentDialogButton.Close,
                XamlRoot = this.Content.XamlRoot,
            };
            _ = await dialog.ShowAsync();
        });
    }

    private static string TruncateStatus(string text, int max = 240)
    {
        if (string.IsNullOrEmpty(text) || text.Length <= max)
        {
            return text;
        }

        return text[..(max - 1)] + "…";
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
            var title = FindJobTitle(args.JobId) ?? "Download";
            MaybeNotifyJob(args.JobId, title, "Download completed", args.Path);
            MaybeRefreshHistory();
        });
    }

    private void OnJobFailed(object? sender, JobFailedEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            DownloadQueueView.MarkJobFailed(args.JobId, args.Error);
            var title = FindJobTitle(args.JobId) ?? "Download";
            MaybeNotifyJob(args.JobId, title, "Download failed", args.Error);
            MaybeRefreshHistory();
        });
    }

    private void OnJobDiscarded(object? sender, JobDiscardedEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            DownloadQueueView.RemoveJob(args.JobId);
        });
    }

    private void OnJobPaused(object? sender, JobLifecycleEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() => DownloadQueueView.MarkJobPaused(args.JobId));
    }

    private void OnJobResumed(object? sender, JobLifecycleEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() => DownloadQueueView.MarkJobResumed(args.JobId));
    }

    private void OnJobRetried(object? sender, JobLifecycleEventArgs args)
    {
        _ = DispatcherQueue.TryEnqueue(() => DownloadQueueView.MarkJobRetried(args.JobId));
    }

    private void OnProcessPendingError(object? sender, string error)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            UrlStatusText.Text = $"Download engine error: {error}";
        });
    }

    private void OnPauseRequested(object? sender, DownloadItemViewModel item)
    {
        try
        {
            _eventService.PauseJob(item.JobId);
            DownloadQueueView.MarkJobPaused(item.JobId);
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Pause failed: {ex.Message}";
        }
    }

    private void OnResumeRequested(object? sender, DownloadItemViewModel item)
    {
        try
        {
            _eventService.ResumeJob(item.JobId);
            DownloadQueueView.MarkJobResumed(item.JobId);
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Resume failed: {ex.Message}";
        }
    }

    private void OnRetryRequested(object? sender, DownloadItemViewModel item)
    {
        try
        {
            _eventService.RetryJob(item.JobId);
            DownloadQueueView.MarkJobRetried(item.JobId);
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Retry failed: {ex.Message}";
        }
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

    private async void OnSettingsClick(object sender, RoutedEventArgs e)
    {
        var panel = new SettingsDialog();
        _openSettingsPanel = panel;
        try
        {
            var audioOnly = await Task.Run(() => _eventService.GetAudioOnlyMode());
            var mediaExtras = await Task.Run(() => _eventService.GetDefaultMediaExtras());
            var cookies = await Task.Run(() => _eventService.GetCookieChannel());
            var proxy = await Task.Run(() => _eventService.GetProxySettings());
            var sidecar = await Task.Run(() => _eventService.GetSidecarUpdateSettings());
            var template = await Task.Run(() => _eventService.GetFilenameTemplate());
            panel.SetAudioOnlyModeDisplay(audioOnly);
            panel.SetMediaExtrasDefaultsDisplay(mediaExtras);
            panel.SetCookieChannelDisplay(cookies);
            panel.SetProxySettingsDisplay(proxy);
            panel.SetSidecarUpdateSettingsDisplay(sidecar);
            panel.SetFilenameTemplateDisplay(template);
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Settings load failed: {ex.Message}";
        }

        panel.AudioOnlyModeChanged += OnAudioOnlyModeChanged;
        panel.MediaExtrasDefaultsChanged += OnMediaExtrasDefaultsChanged;
        panel.CookieChannelChanged += OnCookieChannelChanged;
        panel.ProxySettingsChanged += OnProxySettingsChanged;
        panel.SidecarUpdateSettingsChanged += OnSidecarUpdateSettingsChanged;
        panel.CheckSidecarUpdatesRequested += OnCheckSidecarUpdatesRequested;
        panel.ApplySidecarUpdatesRequested += OnApplySidecarUpdatesRequested;
        panel.PinSidecarVersionRequested += OnPinSidecarVersionRequested;
        panel.RollbackSidecarRequested += OnRollbackSidecarRequested;
        panel.FilenameTemplateChanged += OnFilenameTemplateChanged;

        var dialog = new ContentDialog
        {
            Title = "Settings",
            Content = panel,
            CloseButtonText = "Close",
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = this.Content.XamlRoot,
        };

        try
        {
            await dialog.ShowAsync();
        }
        finally
        {
            panel.AudioOnlyModeChanged -= OnAudioOnlyModeChanged;
            panel.MediaExtrasDefaultsChanged -= OnMediaExtrasDefaultsChanged;
            panel.CookieChannelChanged -= OnCookieChannelChanged;
            panel.ProxySettingsChanged -= OnProxySettingsChanged;
            panel.SidecarUpdateSettingsChanged -= OnSidecarUpdateSettingsChanged;
            panel.CheckSidecarUpdatesRequested -= OnCheckSidecarUpdatesRequested;
            panel.ApplySidecarUpdatesRequested -= OnApplySidecarUpdatesRequested;
            panel.PinSidecarVersionRequested -= OnPinSidecarVersionRequested;
            panel.RollbackSidecarRequested -= OnRollbackSidecarRequested;
            panel.FilenameTemplateChanged -= OnFilenameTemplateChanged;
            _openSettingsPanel = null;
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

    private void OnProxySettingsChanged(object? sender, ProxySettings settings)
    {
        try
        {
            _eventService.SetProxySettings(settings);
            UrlStatusText.Text = "Proxy settings updated";
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Proxy settings failed: {ex.Message}";
        }
    }

    private void OnSidecarUpdateSettingsChanged(object? sender, SidecarUpdateSettings settings)
    {
        try
        {
            _eventService.SetSidecarUpdateSettings(settings);
            UrlStatusText.Text = "Sidecar update settings saved";
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Sidecar settings failed: {ex.Message}";
        }
    }

    private void OnCheckSidecarUpdatesRequested(object? sender, EventArgs e)
    {
        try
        {
            var result = _eventService.CheckSidecarUpdates();
            var message =
                $"Toolchain {result.Toolchain.InstalledVersion ?? "none"}"
                + $" → {result.Toolchain.AvailableVersion ?? "none"}"
                + (result.Toolchain.UpdateAvailable ? " (update)" : "")
                + $"; Plugin {result.Plugin.InstalledVersion ?? "none"}"
                + $" → {result.Plugin.AvailableVersion ?? "none"}"
                + (result.Plugin.UpdateAvailable ? " (update)" : "");
            _openSettingsPanel?.SetSidecarUpdateStatus(message);
            UrlStatusText.Text = "Sidecar update check complete";
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Sidecar check failed: {ex.Message}";
        }
    }

    private void OnApplySidecarUpdatesRequested(object? sender, EventArgs e)
    {
        try
        {
            var result = _eventService.ApplySidecarUpdates();
            _openSettingsPanel?.SetSidecarUpdateStatus($"Apply: {result.Outcome}");
            UrlStatusText.Text = $"Sidecar updates: {result.Outcome}";
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Sidecar apply failed: {ex.Message}";
        }
    }

    private void OnPinSidecarVersionRequested(object? sender, (string Component, string Version) args)
    {
        try
        {
            _eventService.PinSidecarVersion(args.Component, args.Version);
            var settings = _eventService.GetSidecarUpdateSettings();
            _openSettingsPanel?.SetSidecarUpdateSettingsDisplay(settings);
            UrlStatusText.Text = $"Pinned {args.Component} to {args.Version}";
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Pin failed: {ex.Message}";
        }
    }

    private void OnRollbackSidecarRequested(object? sender, string component)
    {
        try
        {
            _eventService.RollbackSidecar(component);
            UrlStatusText.Text = $"Rolled back {component}";
            OnCheckSidecarUpdatesRequested(sender, EventArgs.Empty);
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Rollback failed: {ex.Message}";
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

    private void OnRevealRequested(object? sender, DownloadItemViewModel item)
    {
        try
        {
            OutputFileActions.RevealInFolder(item.OutputPath);
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Reveal failed: {ex.Message}";
        }
    }

    private void OnOpenRequested(object? sender, DownloadItemViewModel item)
    {
        try
        {
            OutputFileActions.OpenFile(item.OutputPath);
        }
        catch (Exception ex)
        {
            UrlStatusText.Text = $"Open failed: {ex.Message}";
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
                FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
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
    /// Called by App when an unhandled UI exception is swallowed.
    /// </summary>
    public void ReportUnhandledError(string message)
    {
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            UrlStatusText.Text = $"Unexpected error: {message}";
        });
    }

    /// <summary>
    /// Called by ProbeSheetControl when the user confirms a selection.
    /// Switches back to the download queue view.
    /// </summary>
    public void OnProbeConfirmed()
    {
        if (_historyTabActive)
        {
            HistoryView.Visibility = Visibility.Visible;
            DownloadQueueView.Visibility = Visibility.Collapsed;
        }
        else
        {
            DownloadQueueView.Visibility = Visibility.Visible;
            HistoryView.Visibility = Visibility.Collapsed;
        }
        ProbeSheetView.Visibility = Visibility.Collapsed;
        UrlInput?.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
    }
}
