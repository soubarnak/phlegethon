using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Phlegethon.Services;

namespace Phlegethon.Controls;

public class HistoryItemViewModel : INotifyPropertyChanged
{
    private string _title = "";
    private string _subtitle = "";
    private string _mediaId = "";
    private string _url = "";
    private string _destination = "";
    private string _outcome = "Completed";

    public event PropertyChangedEventHandler? PropertyChanged;

    public string MediaId
    {
        get => _mediaId;
        set => SetField(ref _mediaId, value);
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

    public string Url
    {
        get => _url;
        set => SetField(ref _url, value);
    }

    public string Destination
    {
        get => _destination;
        set => SetField(ref _destination, value);
    }

    public string Outcome
    {
        get => _outcome;
        set
        {
            if (SetField(ref _outcome, value))
            {
                PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(CanOpen)));
            }
        }
    }

    public bool CanOpen => !string.Equals(Outcome, "Failed", StringComparison.OrdinalIgnoreCase);

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

public sealed partial class HistoryControl : UserControl
{
    private readonly ObservableCollection<HistoryItemViewModel> _items = new();

    public HistoryControl()
    {
        InitializeComponent();
        HistoryList.ItemsSource = _items;
    }

    public event EventHandler<HistoryItemViewModel>? RevealRequested;
    public event EventHandler<HistoryItemViewModel>? OpenRequested;
    public event EventHandler<HistoryItemViewModel>? ReDownloadRequested;
    public event EventHandler<HistoryItemViewModel>? RemoveRequested;

    public void SetEntries(IReadOnlyList<ArchiveEntry> entries, string filter)
    {
        _items.Clear();
        foreach (var entry in entries)
        {
            if (filter != "all" && !string.Equals(entry.Outcome, filter, StringComparison.Ordinal))
            {
                continue;
            }

            _items.Add(new HistoryItemViewModel
            {
                MediaId = entry.MediaId,
                Title = entry.Title,
                Url = entry.Url,
                Destination = entry.Destination,
                Outcome = entry.Outcome,
                Subtitle = $"{entry.Outcome} · {FormatTimestamp(entry.CompletedAtUnixSecs)}",
            });
        }

        EmptyStateText.Visibility = _items.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        HistoryList.Visibility = _items.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        if (_items.Count == 0)
        {
            EmptyStateText.Text = filter switch
            {
                "Failed" => "No failed downloads.",
                "Completed" => "No completed downloads.",
                _ => "No completed downloads yet.",
            };
        }
    }

    public string CurrentFilter()
    {
        if (HistoryFilterBox.SelectedItem is ComboBoxItem item
            && item.Tag is string tag)
        {
            return tag;
        }
        return "all";
    }

    private void OnHistoryFilterChanged(object sender, SelectionChangedEventArgs e)
    {
        FilterChanged?.Invoke(this, EventArgs.Empty);
    }

    public event EventHandler? FilterChanged;

    private void OnRevealClick(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: HistoryItemViewModel item })
        {
            RevealRequested?.Invoke(this, item);
        }
    }

    private void OnOpenClick(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: HistoryItemViewModel item })
        {
            OpenRequested?.Invoke(this, item);
        }
    }

    private void OnReDownloadClick(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: HistoryItemViewModel item })
        {
            ReDownloadRequested?.Invoke(this, item);
        }
    }

    private void OnRemoveClick(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: HistoryItemViewModel item })
        {
            RemoveRequested?.Invoke(this, item);
        }
    }

    private static string FormatTimestamp(ulong unixSecs)
    {
        var d = DateTimeOffset.FromUnixTimeSeconds((long)unixSecs).LocalDateTime;
        var now = DateTime.Now;
        var diff = now - d;
        if (diff < TimeSpan.FromMinutes(1)) return "Just now";
        if (diff < TimeSpan.FromHours(1)) return $"{(int)diff.TotalMinutes}m ago";
        if (d.Date == now.Date)
            return d.ToString("t");
        if (d.Date == now.Date.AddDays(-1))
            return "Yesterday " + d.ToString("t");
        return d.ToString(d.Year == now.Year ? "MMM d" : "MMM d, yyyy");
    }
}
