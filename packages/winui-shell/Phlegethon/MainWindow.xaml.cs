using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Phlegethon.Services;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;

namespace Phlegethon;

public sealed partial class MainWindow : Window
{
    private readonly EventChannelService _eventService;

    public MainWindow()
    {
        this.InitializeComponent();
        _eventService = new EventChannelService();
        _eventService.OnProbeResult += OnProbeResult;
        _eventService.OnJobProgress += OnJobProgress;
        _eventService.OnJobCompleted += OnJobCompleted;
        _eventService.OnJobFailed += OnJobFailed;

        // Start the event loop background task
        _ = _eventService.StartEventLoopAsync();

        // Register paste/drop handlers
        this.Content.AllowDrop = true;
        this.Content.Drop += OnDrop;
        this.Content.PrePaste += OnPaste;

        // Wire up probe sheet
        ProbeSheetView.Canceled += (_, _) => OnProbeConfirmed();
        ProbeSheetView.Confirmed += (_, json) =>
        {
            // Pass selection to Core
            _ = Task.Run(() =>
            {
                try
                {
                    var resultJson = _eventService.Core.ConfirmProbeSelection(json);
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
    // Paste / Drop
    // -----------------------------------------------------------------------

    /// <summary>
    /// Handle a keyboard paste (Ctrl+V) — read clipboard text and enqueue.
    /// </summary>
    private async void OnPaste(object sender, RoutedEventArgs e)
    {
        // Use keyboard accelerators for Ctrl+V or handle via clipboard polling.
        // The simpler approach is a keyboard accelerator on the window.
        var dataPackage = Clipboard.GetContent();
        if (dataPackage.Contains(StandardDataFormats.Text))
        {
            var text = await dataPackage.GetTextAsync();
            if (Uri.TryCreate(text, UriKind.Absolute, out var uri) &&
                (uri.Scheme == "http" || uri.Scheme == "https"))
            {
                _eventService.EnqueueUrl(text);
                UrlStatusText.Text = $"Enqueued: {text}";
            }
        }
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
            if (Uri.TryCreate(text, UriKind.Absolute, out var uri) &&
                (uri.Scheme == "http" || uri.Scheme == "https"))
            {
                _eventService.EnqueueUrl(text);
                UrlStatusText.Text = $"Enqueued from drop: {text}";
            }
        }
        else if (e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            var items = await e.DataView.GetStorageItemsAsync();
            foreach (var item in items)
            {
                if (item is StorageFile file && file.FileType == ".url")
                {
                    var text = await FileIO.ReadTextAsync(file);
                    _eventService.EnqueueUrl(text);
                    UrlStatusText.Text = $"Enqueued from file: {text}";
                }
            }
        }
    }

    /// <summary>
    /// Handle "Add URL" button click — show a simple dialog for URL entry.
    /// </summary>
    private async void OnAddUrlClick(object sender, RoutedEventArgs e)
    {
        var dialog = new ContentDialog
        {
            Title = "Add Download URL",
            Content = new TextBox
            {
                PlaceholderText = "https://example.com/video.mp4",
                AcceptsReturn = false,
                FontFamily = (Windows.UI.Xaml.Media.FontFamily)Application.Current.Resources["GeistMonoFont"],
            },
            PrimaryButtonText = "Add",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = this.Content.XamlRoot,
        };

        var result = await dialog.ShowAsync();
        if (result == ContentDialogResult.Primary)
        {
            var textBox = (TextBox)dialog.Content;
            var url = textBox.Text.Trim();
            if (!string.IsNullOrEmpty(url))
            {
                _eventService.EnqueueUrl(url);
                UrlStatusText.Text = $"Enqueued: {url}";
            }
        }
    }

    // -----------------------------------------------------------------------
    // Event handlers
    // -----------------------------------------------------------------------

    private void OnProbeResult(object? sender, ProbeResultEventArgs args)
    {
        // Switch to the Probe-then-Confirm sheet
        _ = DispatcherQueue.TryEnqueue(() =>
        {
            ProbeSheetView.ShowProbeResult(args.Url, args.Items, args.SourceName);
            DownloadQueueView.Visibility = Visibility.Collapsed;
            ProbeSheetView.Visibility = Visibility.Visible;
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
