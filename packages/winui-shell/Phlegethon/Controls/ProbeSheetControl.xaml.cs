using System;
using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace Phlegethon.Controls;

/// <summary>
/// View model for a quality option row.
/// </summary>
public class QualityViewModel
{
    public string Id { get; set; } = "";
    public string Label { get; set; } = "";
}

/// <summary>
/// View model for a media item group.
/// </summary>
public class MediaGroupViewModel
{
    public string Id { get; set; } = "";
    public string Title { get; set; } = "";
    public string Url { get; set; } = "";
    public List<QualityViewModel> Qualities { get; set; } = new();
    public bool IsSelected { get; set; }
    public QualityViewModel? SelectedQuality { get; set; }
}

/// <summary>
/// Dense probe-then-confirm sheet (variant A).
///
/// Shows the probe result as a compact list of media items with
/// quality dropdowns. User selects what to download, then confirms.
/// </summary>
public sealed partial class ProbeSheetControl : UserControl
{
    private string _probeUrl = "";
    private readonly List<MediaGroupViewModel> _groups = new();

    public ProbeSheetControl()
    {
        this.InitializeComponent();
    }

    /// <summary>
    /// Show probe result from the Download Core.
    /// </summary>
    public void ShowProbeResult(string url, List<MediaItem> items, string? sourceName)
    {
        _probeUrl = url;
        _groups.Clear();
        ItemsPanel.Children.Clear();

        SourceLabel.Text = sourceName != null
            ? $"{sourceName} — {url}"
            : $"Probed: {url}";

        foreach (var item in items)
        {
            var group = new MediaGroupViewModel
            {
                Id = item.Id,
                Title = item.Title,
                Url = item.Url,
                IsSelected = false,
            };

            foreach (var q in item.Qualities)
            {
                group.Qualities.Add(new QualityViewModel { Id = q.Id, Label = q.Label });
            }

            if (group.Qualities.Count > 0)
            {
                group.SelectedQuality = group.Qualities[0];
            }

            _groups.Add(group);
            ItemsPanel.Children.Add(BuildMediaGroupRow(group));
        }

        UpdateSelectionSummary();
    }

    // -----------------------------------------------------------------------
    // UI building
    // -----------------------------------------------------------------------

    private Border BuildMediaGroupRow(MediaGroupViewModel group)
    {
        var row = new Border
        {
            BorderBrush = (Brush)Application.Current.Resources["GeistBorderBrush"],
            BorderThickness = new Thickness(0, 0, 0, 1),
            Height = 56,
            Padding = new Thickness(12, 0),
        };

        var grid = new Grid { ColumnSpacing = 8 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        // Checkbox
        var checkBox = new CheckBox
        {
            VerticalAlignment = VerticalAlignment.Center,
            IsChecked = group.IsSelected,
        };
        checkBox.Checked += (_, _) => { group.IsSelected = true; UpdateSelectionSummary(); };
        checkBox.Unchecked += (_, _) => { group.IsSelected = false; UpdateSelectionSummary(); };
        Grid.SetColumn(checkBox, 0);
        grid.Children.Add(checkBox);

        // Title
        var titleBlock = new TextBlock
        {
            Text = group.Title,
            FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
            FontSize = 14,
            FontWeight = (Windows.UI.Text.FontWeight)Application.Current.Resources["GeistTypeWeightMedium"],
            Foreground = (Brush)Application.Current.Resources["GeistTextPrimaryBrush"],
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            TextWrapping = TextTrimming.TextWrapping.NoWrap,
            MaxLines = 1,
        };
        Grid.SetColumn(titleBlock, 1);
        grid.Children.Add(titleBlock);

        // Quality selector (if multiple qualities)
        if (group.Qualities.Count > 1)
        {
            var qualityCombo = new ComboBox
            {
                VerticalAlignment = VerticalAlignment.Center,
                MinWidth = 90,
                Height = 28,
                FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
                FontSize = 12,
                PlaceholderText = "Quality",
                ItemsSource = group.Qualities,
                SelectedItem = group.SelectedQuality,
            };
            qualityCombo.SelectionChanged += (_, args) =>
            {
                group.SelectedQuality = (QualityViewModel)args.AddedItems[0];
            };
            Grid.SetColumn(qualityCombo, 2);
            grid.Children.Add(qualityCombo);
        }
        else if (group.Qualities.Count == 1)
        {
            var labelBlock = new TextBlock
            {
                Text = group.Qualities[0].Label,
                FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
                FontSize = 12,
                Foreground = (Brush)Application.Current.Resources["GeistTextSecondaryBrush"],
                VerticalAlignment = VerticalAlignment.Center,
            };
            Grid.SetColumn(labelBlock, 2);
            grid.Children.Add(labelBlock);
        }

        // Url button
        var urlButton = new Button
        {
            Content = "URL",
            FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
            FontSize = 12,
            Height = 28,
            MinWidth = 40,
            Padding = new Thickness(6, 0),
            VerticalAlignment = VerticalAlignment.Center,
        };
        urlButton.Click += (_, _) =>
        {
            // Open URL in default browser
            var uri = new Uri(group.Url);
            _ = Windows.System.Launcher.LaunchUriAsync(uri);
        };
        Grid.SetColumn(urlButton, 3);
        grid.Children.Add(urlButton);

        row.Child = grid;
        return row;
    }

    // -----------------------------------------------------------------------
    // Selection summary
    // -----------------------------------------------------------------------

    private void UpdateSelectionSummary()
    {
        var count = 0;
        foreach (var g in _groups)
        {
            if (g.IsSelected) count++;
        }

        SelectionSummary.Text = count switch
        {
            0 => "No selection",
            1 => "1 item selected",
            _ => $"{count} items selected",
        };

        ConfirmButton.IsEnabled = count > 0;
    }

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------

    /// <summary>
    /// Fired when the user cancels the probe sheet.
    /// </summary>
    public event EventHandler? Canceled;

    /// <summary>
    /// Fired when the user confirms a selection with their chosen qualities.
    /// The string is a JSON object: { url, items: [{ item_id, quality_id }] }.
    /// </summary>
    public event EventHandler<string>? Confirmed;

    // -----------------------------------------------------------------------
    // Event handlers
    // -----------------------------------------------------------------------

    private void OnCancelClick(object sender, RoutedEventArgs e)
    {
        Canceled?.Invoke(this, EventArgs.Empty);
    }

    private async void OnConfirmClick(object sender, RoutedEventArgs e)
    {
        // Build JSON selection expected by ProbeSelection in download-core.
        var items = new List<object>();
        foreach (var g in _groups)
        {
            if (g.IsSelected)
            {
                items.Add(new
                {
                    item_id = g.Id,
                    quality_id = g.SelectedQuality?.Id ?? "",
                });
            }
        }

        var selectionJson = System.Text.Json.JsonSerializer.Serialize(new
        {
            url = _probeUrl,
            items,
        });
        Confirmed?.Invoke(this, selectionJson);

        var dialog = new ContentDialog
        {
            Title = "Downloading",
            Content = $"{items.Count} item(s) added to queue.",
            CloseButtonText = "OK",
            XamlRoot = this.XamlRoot,
        };
        await dialog.ShowAsync();
    }
}
