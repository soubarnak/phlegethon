using System;
using Microsoft.UI.Xaml.Data;

namespace Phlegethon;

/// <summary>
/// Maps a 0–100 progress percent to a pixel width for compact queue bars.
/// Optional <see cref="IValueConverter.Convert"/> parameter sets the max width (default 80).
/// </summary>
public sealed class PercentToWidthConverter : IValueConverter
{
    private const double DefaultMaxWidth = 80.0;

    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is not double percent)
        {
            return 0.0;
        }

        var maxWidth = DefaultMaxWidth;
        if (parameter is string text && double.TryParse(text, out var parsed))
        {
            maxWidth = parsed;
        }
        else if (parameter is double direct)
        {
            maxWidth = direct;
        }

        var clamped = Math.Clamp(percent, 0.0, 100.0);
        return clamped / 100.0 * maxWidth;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotSupportedException();
}
