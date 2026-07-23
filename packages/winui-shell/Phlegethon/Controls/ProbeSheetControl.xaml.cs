using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Phlegethon.Services;
using Phlegethon.Strings;
using Windows.UI.Text;

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
    public bool IsArchived { get; set; }
    public bool QualitiesEnriched { get; set; }
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
    private readonly Dictionary<string, CheckBox> _rowCheckBoxes = new();
    private string? _saveToDestination;
    private int _previewRequestId;
    private MediaExtrasOptions _mediaExtras = new();
    private readonly Dictionary<string, CheckBox> _mediaExtraBoxes = new();
    private TextBlock? _ffmpegNote;

    /// <summary>
    /// Resolve Core Job Destination preview for selection JSON.
    /// Returns <c>path_display</c>, or null to fall back to raw Save-to / default.
    /// </summary>
    public Func<string, string?>? ResolveDestinationPreview { get; set; }

    /// <summary>
    /// Resolve the Default Quality Policy selection for a row's qualities.
    /// </summary>
    public Func<IReadOnlyList<QualityViewModel>, string?>? ResolveDefaultQuality { get; set; }

    /// <summary>Load settings-backed media extras defaults for this probe.</summary>
    public Func<MediaExtrasOptions>? ResolveDefaultMediaExtras { get; set; }

    /// <summary>Report ffmpeg availability for embed/remux toggles.</summary>
    public Func<MediaExtrasCapabilities>? ResolveMediaExtrasCapabilities { get; set; }

    /// <summary>Fetch exact formats for one probe row (updates Core pending probe).</summary>
    public Func<string, string, Task<List<Quality>>>? ResolveItemQualitiesAsync { get; set; }

    public ProbeSheetControl()
    {
        this.InitializeComponent();
        ApplyLocalizedStrings();
    }

    private void ApplyLocalizedStrings()
    {
        ProbeTitleText.Text = ShellStrings.Get("probe.title");
        SelectAllButton.Content = ShellStrings.Get("probe.selectAll");
        AutomationProperties.SetName(SelectAllButton, ShellStrings.Get("probe.selectAll"));
        DeselectAllButton.Content = ShellStrings.Get("probe.deselectAll");
        AutomationProperties.SetName(DeselectAllButton, ShellStrings.Get("probe.deselectAll"));
        CancelButton.Content = ShellStrings.Get("probe.cancel");
        AutomationProperties.SetName(CancelButton, ShellStrings.Get("probe.cancel"));
        SaveToButton.Content = ShellStrings.Get("probe.saveTo");
        AutomationProperties.SetName(SaveToButton, ShellStrings.Get("probe.saveTo"));
        ConfirmButton.Content = ShellStrings.Get("probe.confirm");
        AutomationProperties.SetName(ConfirmButton, ShellStrings.Get("probe.confirm"));
        SaveToPathLabel.Text = ShellStrings.Get("probe.libraryRootDefault");
        SelectionSummary.Text = ShellStrings.Get("probe.noSelection");
        SourceLabel.Text = ShellStrings.Get("probe.probing");
        ProbeSheetRoot.TabFocusNavigation = KeyboardNavigationMode.Cycle;
    }

    /// <summary>Move keyboard focus to the probe sheet header for keyboard users.</summary>
    public void FocusInitial()
    {
        CancelButton?.Focus(FocusState.Programmatic);
    }

    /// <summary>
    /// Absolute Job Destination override, or null for Library Root default.
    /// </summary>
    public string? SaveToDestination => _saveToDestination;

    /// <summary>
    /// Show probe result from the Download Core.
    /// </summary>
    public void ShowProbeResult(string url, List<MediaItem> items, string? sourceName)
    {
        _probeUrl = url;
        _saveToDestination = null;
        _groups.Clear();
        _rowCheckBoxes.Clear();
        ItemsPanel.Children.Clear();
        UpdateSaveToLabel();

        SourceLabel.Text = sourceName != null
            ? ShellStrings.Format("probe.sourceAndUrl", new Dictionary<string, string>
            {
                ["source"] = sourceName,
                ["url"] = url,
            })
            : ShellStrings.Format("probe.probed", new Dictionary<string, string> { ["url"] = url });

        _mediaExtras = ResolveDefaultMediaExtras?.Invoke() ?? new MediaExtrasOptions();
        BuildMediaExtrasPanel();

        foreach (var item in items)
        {
            var group = new MediaGroupViewModel
            {
                Id = item.Id,
                Title = item.Title,
                Url = item.Url,
                IsArchived = item.Archived,
                IsSelected = !item.Archived,
            };

            foreach (var q in item.Qualities)
            {
                group.Qualities.Add(new QualityViewModel { Id = q.Id, Label = FormatQualityLabel(q) });
            }

            if (group.Qualities.Count > 0)
            {
                var defaultId = ResolveDefaultQuality?.Invoke(group.Qualities);
                group.SelectedQuality = group.Qualities.FirstOrDefault(q => q.Id == defaultId)
                    ?? group.Qualities[0];
            }

            _groups.Add(group);
            ItemsPanel.Children.Add(BuildMediaGroupRow(group));
        }

        UpdateSelectionSummary();
    }

    private void BuildMediaExtrasPanel()
    {
        MediaExtrasPanel.Children.Clear();
        _mediaExtraBoxes.Clear();

        var caps = ResolveMediaExtrasCapabilities?.Invoke() ?? new MediaExtrasCapabilities();
        var embedAllowed = caps.FfmpegAvailable;

        var title = new TextBlock
        {
            Text = ShellStrings.Get("probe.mediaOptions"),
            FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
            FontSize = 12,
            FontWeight = GeistFontWeight("GeistTypeWeightMedium"),
            Foreground = (Brush)Application.Current.Resources["GeistTextPrimaryBrush"],
        };
        MediaExtrasPanel.Children.Add(title);

        if (!embedAllowed && !string.IsNullOrEmpty(caps.EmbedDisabledReason))
        {
            _ffmpegNote = new TextBlock
            {
                Text = caps.EmbedDisabledReason,
                FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
                FontSize = 11,
                Foreground = (Brush)Application.Current.Resources["GeistTextSecondaryBrush"],
                TextWrapping = TextWrapping.WrapWholeWords,
            };
            MediaExtrasPanel.Children.Add(_ffmpegNote);
        }

        var grid = new Grid { ColumnSpacing = 12, RowSpacing = 4 };
        for (var i = 0; i < 4; i++)
        {
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        }

        AddMediaExtraToggle(grid, 0, 0, ShellStrings.Get("probe.media.writeSubs"), nameof(MediaExtrasOptions.WriteSubs), _mediaExtras.WriteSubs, true);
        AddMediaExtraToggle(grid, 1, 0, ShellStrings.Get("probe.media.embedSubs"), nameof(MediaExtrasOptions.EmbedSubs), _mediaExtras.EmbedSubs, embedAllowed);
        AddMediaExtraToggle(grid, 2, 0, ShellStrings.Get("probe.media.writeThumbnail"), nameof(MediaExtrasOptions.WriteThumbnail), _mediaExtras.WriteThumbnail, true);
        AddMediaExtraToggle(grid, 3, 0, ShellStrings.Get("probe.media.embedThumbnail"), nameof(MediaExtrasOptions.EmbedThumbnail), _mediaExtras.EmbedThumbnail, embedAllowed);
        AddMediaExtraToggle(grid, 0, 1, ShellStrings.Get("probe.media.writeMetadata"), nameof(MediaExtrasOptions.WriteMetadata), _mediaExtras.WriteMetadata, true);
        AddMediaExtraToggle(grid, 1, 1, ShellStrings.Get("probe.media.embedMetadata"), nameof(MediaExtrasOptions.EmbedMetadata), _mediaExtras.EmbedMetadata, embedAllowed);
        AddMediaExtraToggle(grid, 2, 1, ShellStrings.Get("probe.media.writeChapters"), nameof(MediaExtrasOptions.WriteChapters), _mediaExtras.WriteChapters, true);
        AddMediaExtraToggle(grid, 3, 1, ShellStrings.Get("probe.media.embedChapters"), nameof(MediaExtrasOptions.EmbedChapters), _mediaExtras.EmbedChapters, embedAllowed);
        AddMediaExtraToggle(grid, 0, 2, ShellStrings.Get("probe.media.remux"), nameof(MediaExtrasOptions.Remux), _mediaExtras.Remux, embedAllowed);

        MediaExtrasPanel.Children.Add(grid);
    }

    private void AddMediaExtraToggle(
        Grid grid,
        int column,
        int row,
        string label,
        string key,
        bool isChecked,
        bool enabled)
    {
        while (grid.RowDefinitions.Count <= row)
        {
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        }

        var box = new CheckBox
        {
            Content = label,
            IsChecked = enabled && isChecked,
            IsEnabled = enabled,
            FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
            FontSize = 11,
        };
        Grid.SetColumn(box, column);
        Grid.SetRow(box, row);
        grid.Children.Add(box);
        _mediaExtraBoxes[key] = box;
    }

    private MediaExtrasOptions ReadMediaExtrasFromUi()
    {
        bool Read(string key) =>
            _mediaExtraBoxes.TryGetValue(key, out var box) && box.IsChecked == true;

        return new MediaExtrasOptions
        {
            WriteSubs = Read(nameof(MediaExtrasOptions.WriteSubs)),
            EmbedSubs = Read(nameof(MediaExtrasOptions.EmbedSubs)),
            WriteChapters = Read(nameof(MediaExtrasOptions.WriteChapters)),
            EmbedChapters = Read(nameof(MediaExtrasOptions.EmbedChapters)),
            WriteThumbnail = Read(nameof(MediaExtrasOptions.WriteThumbnail)),
            EmbedThumbnail = Read(nameof(MediaExtrasOptions.EmbedThumbnail)),
            WriteMetadata = Read(nameof(MediaExtrasOptions.WriteMetadata)),
            EmbedMetadata = Read(nameof(MediaExtrasOptions.EmbedMetadata)),
            Remux = Read(nameof(MediaExtrasOptions.Remux)),
        };
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
            Padding = new Thickness(12, 0, 12, 0),
        };

        var grid = new Grid { ColumnSpacing = 8 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var checkBox = new CheckBox
        {
            VerticalAlignment = VerticalAlignment.Center,
            IsChecked = group.IsSelected,
        };
        AutomationProperties.SetName(
            checkBox,
            ShellStrings.Format("probe.selectItem", new Dictionary<string, string> { ["title"] = group.Title }));
        checkBox.Checked += (_, _) => { group.IsSelected = true; UpdateSelectionSummary(); };
        checkBox.Unchecked += (_, _) => { group.IsSelected = false; UpdateSelectionSummary(); };
        Grid.SetColumn(checkBox, 0);
        grid.Children.Add(checkBox);
        _rowCheckBoxes[group.Id] = checkBox;

        var titleBlock = new TextBlock
        {
            Text = group.Title,
            FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
            FontSize = 14,
            FontWeight = GeistFontWeight("GeistTypeWeightMedium"),
            Foreground = (Brush)Application.Current.Resources["GeistTextPrimaryBrush"],
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            TextWrapping = TextWrapping.NoWrap,
            MaxLines = 1,
        };
        Grid.SetColumn(titleBlock, 1);

        var titlePanel = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            VerticalAlignment = VerticalAlignment.Center,
        };
        titlePanel.Children.Add(titleBlock);
        if (group.IsArchived)
        {
            titlePanel.Children.Add(new Border
            {
                Background = (Brush)Application.Current.Resources["GeistSurfaceBrush"],
                BorderBrush = (Brush)Application.Current.Resources["GeistBorderBrush"],
                BorderThickness = new Thickness(1),
                CornerRadius = new CornerRadius(4),
                Padding = new Thickness(6, 2, 6, 2),
                Child = new TextBlock
                {
                    Text = ShellStrings.Get("probe.downloaded"),
                    FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
                    FontSize = 11,
                    Foreground = (Brush)Application.Current.Resources["GeistTextSecondaryBrush"],
                },
            });
        }
        Grid.SetColumn(titlePanel, 1);
        grid.Children.Add(titlePanel);

        if (group.Qualities.Count > 1)
        {
            var qualityCombo = new ComboBox
            {
                VerticalAlignment = VerticalAlignment.Center,
                MinWidth = 140,
                Height = 28,
                FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
                FontSize = 12,
                PlaceholderText = ShellStrings.Get("probe.quality"),
                ItemsSource = group.Qualities,
                SelectedItem = group.SelectedQuality,
                DisplayMemberPath = nameof(QualityViewModel.Label),
            };
            AutomationProperties.SetName(
                qualityCombo,
                ShellStrings.Format("probe.qualityFor", new Dictionary<string, string> { ["title"] = group.Title }));
            qualityCombo.SelectionChanged += (_, args) =>
            {
                if (args.AddedItems.Count > 0)
                {
                    group.SelectedQuality = (QualityViewModel)args.AddedItems[0];
                }
            };
            qualityCombo.DropDownOpened += async (_, _) =>
            {
                await EnrichQualitiesIfNeededAsync(group, qualityCombo);
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

        var urlButton = new Button
        {
            Content = "URL",
            FontFamily = (FontFamily)Application.Current.Resources["GeistSansFont"],
            FontSize = 12,
            Height = 28,
            MinWidth = 40,
            Padding = new Thickness(6, 0, 6, 0),
            VerticalAlignment = VerticalAlignment.Center,
        };
        AutomationProperties.SetName(
            urlButton,
            ShellStrings.Format("probe.openUrl", new Dictionary<string, string> { ["title"] = group.Title }));
        urlButton.Click += (_, _) =>
        {
            var uri = new Uri(group.Url);
            _ = Windows.System.Launcher.LaunchUriAsync(uri);
        };
        Grid.SetColumn(urlButton, 3);
        grid.Children.Add(urlButton);

        row.Child = grid;
        return row;
    }

    // -----------------------------------------------------------------------
    // Selection / Save to…
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
            0 => ShellStrings.Get("probe.noSelection"),
            1 => ShellStrings.Get("probe.selection.one"),
            _ => ShellStrings.Format("probe.selection.many", new Dictionary<string, string>
            {
                ["count"] = count.ToString(),
            }),
        };

        ConfirmButton.IsEnabled = count > 0 && SelectedItemsHaveQuality();
        UpdateSaveToLabel();
    }

    private bool SelectedItemsHaveQuality()
    {
        foreach (var g in _groups)
        {
            if (!g.IsSelected)
            {
                continue;
            }

            if (string.IsNullOrEmpty(g.SelectedQuality?.Id))
            {
                return false;
            }
        }

        return _groups.Any(g => g.IsSelected);
    }

    private object BuildSelectionPayload()
    {
        var items = new List<object>();
        foreach (var g in _groups)
        {
            if (!g.IsSelected) continue;
            items.Add(new
            {
                item_id = g.Id,
                quality_id = g.SelectedQuality?.Id ?? "",
                title = g.Title,
            });
        }

        return _saveToDestination != null
            ? new { url = _probeUrl, items, destination = _saveToDestination, media_extras = ReadMediaExtrasFromUi() }
            : new { url = _probeUrl, items, media_extras = ReadMediaExtrasFromUi() };
    }

    private async void UpdateSaveToLabel()
    {
        var selected = 0;
        foreach (var g in _groups)
        {
            if (g.IsSelected) selected++;
        }

        var fallback = _saveToDestination ?? ShellStrings.Get("probe.libraryRootDefault");
        var multiNote = selected > 1 ? " (each item under its own subfolder)" : "";

        if (selected == 0 || ResolveDestinationPreview == null || string.IsNullOrEmpty(_probeUrl))
        {
            SaveToPathLabel.Text = selected > 1 ? $"{fallback}{multiNote}" : fallback;
            return;
        }

        var selectionJson = System.Text.Json.JsonSerializer.Serialize(BuildSelectionPayload());
        var requestId = ++_previewRequestId;
        string? pathDisplay = null;
        try
        {
            pathDisplay = await Task.Run(() => ResolveDestinationPreview?.Invoke(selectionJson));
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"Probe preview error: {ex.Message}");
        }

        if (requestId != _previewRequestId) return;

        if (!string.IsNullOrEmpty(pathDisplay))
        {
            SaveToPathLabel.Text = $"{pathDisplay}{multiNote}";
        }
        else
        {
            SaveToPathLabel.Text = selected > 1 ? $"{fallback}{multiNote}" : fallback;
        }
    }

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------

    /// <summary>Current probe URL (for cancel / clear pending probe).</summary>
    public string ProbeUrl => _probeUrl;

    /// <summary>
    /// Fired when Save-to folder picker fails.
    /// </summary>
    public event EventHandler<string>? SaveToFailed;

    /// <summary>
    /// Fired when the user cancels the probe sheet.
    /// </summary>
    public event EventHandler? Canceled;

    /// <summary>
    /// Fired when the user confirms a selection with their chosen qualities.
    /// JSON: { url, items: [{ item_id, quality_id }], destination? }.
    /// </summary>
    public event EventHandler<string>? Confirmed;

    // -----------------------------------------------------------------------
    // Event handlers
    // -----------------------------------------------------------------------

    private void OnCancelClick(object sender, RoutedEventArgs e)
    {
        Canceled?.Invoke(this, EventArgs.Empty);
    }

    private void OnSelectAllClick(object sender, RoutedEventArgs e)
    {
        SetAllSelected(true);
    }

    private void OnDeselectAllClick(object sender, RoutedEventArgs e)
    {
        SetAllSelected(false);
    }

    private void SetAllSelected(bool selected)
    {
        foreach (var group in _groups)
        {
            group.IsSelected = selected;
            if (_rowCheckBoxes.TryGetValue(group.Id, out var box))
            {
                box.IsChecked = selected;
            }
        }

        UpdateSelectionSummary();
    }

    private async Task EnrichQualitiesIfNeededAsync(MediaGroupViewModel group, ComboBox qualityCombo)
    {
        if (group.QualitiesEnriched || ResolveItemQualitiesAsync == null || string.IsNullOrEmpty(_probeUrl))
        {
            return;
        }

        qualityCombo.IsEnabled = false;
        try
        {
            var qualities = await ResolveItemQualitiesAsync(_probeUrl, group.Id);
            if (qualities.Count == 0)
            {
                return;
            }

            ApplyQualities(group, qualities);
            qualityCombo.ItemsSource = null;
            qualityCombo.ItemsSource = group.Qualities;
            qualityCombo.SelectedItem = group.SelectedQuality;
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"Quality enrich error: {ex.Message}");
        }
        finally
        {
            qualityCombo.IsEnabled = true;
        }
    }

    private void OnSaveToClick(object sender, RoutedEventArgs e)
    {
        try
        {
            var path = FolderPickerService.PickFolder(GetParentWindow(), ShellStrings.Get("probe.saveTo"));
            if (!string.IsNullOrEmpty(path))
            {
                _saveToDestination = path;
                UpdateSaveToLabel();
            }
        }
        catch (Exception ex)
        {
            SaveToFailed?.Invoke(this, ex.Message);
        }
    }

    private Window GetParentWindow()
    {
        if (Application.Current is App app && app.MainWindow is Window main)
        {
            return main;
        }

        throw new InvalidOperationException("No parent window for folder picker");
    }

    private void OnConfirmClick(object sender, RoutedEventArgs e)
    {
        _ = ConfirmSelectionAsync();
    }

    private async Task ConfirmSelectionAsync()
    {
        ConfirmButton.IsEnabled = false;
        try
        {
            await EnrichSelectedQualitiesAsync();

            if (!SelectedItemsHaveQuality())
            {
                return;
            }

            var selectionJson = System.Text.Json.JsonSerializer.Serialize(BuildSelectionPayload());
            Confirmed?.Invoke(this, selectionJson);
        }
        finally
        {
            ConfirmButton.IsEnabled = SelectedItemsHaveQuality();
        }
    }

    private async Task EnrichSelectedQualitiesAsync()
    {
        if (ResolveItemQualitiesAsync == null || string.IsNullOrEmpty(_probeUrl))
        {
            return;
        }

        foreach (var group in _groups)
        {
            if (!group.IsSelected || group.QualitiesEnriched)
            {
                continue;
            }

            try
            {
                var qualities = await ResolveItemQualitiesAsync(_probeUrl, group.Id);
                if (qualities.Count == 0)
                {
                    continue;
                }

                ApplyQualities(group, qualities);
            }
            catch (Exception)
            {
            }
        }
    }

    private void ApplyQualities(MediaGroupViewModel group, List<Quality> qualities)
    {
        var previousId = group.SelectedQuality?.Id;
        group.Qualities.Clear();
        foreach (var q in qualities)
        {
            group.Qualities.Add(new QualityViewModel
            {
                Id = q.Id,
                Label = FormatQualityLabel(q),
            });
        }

        var defaultId = ResolveDefaultQuality?.Invoke(group.Qualities);
        group.SelectedQuality = group.Qualities.FirstOrDefault(q => q.Id == previousId)
            ?? group.Qualities.FirstOrDefault(q => q.Id == defaultId)
            ?? group.Qualities.FirstOrDefault();
        group.QualitiesEnriched = true;
    }

    private static string FormatQualityLabel(Quality q)
    {
        if (q.SizeBytes is > 0)
        {
            var mb = q.SizeBytes.Value / (1024.0 * 1024.0);
            return $"{q.Label} ({mb:0.#} MB)";
        }

        return q.Label;
    }

    private static FontWeight GeistFontWeight(string resourceKey)
    {
        var value = Application.Current.Resources[resourceKey];
        return value switch
        {
            double weight => new FontWeight((ushort)weight),
            int weight => new FontWeight((ushort)weight),
            FontWeight weight => weight,
            _ => new FontWeight(400),
        };
    }
}
