namespace Phlegethon;

/// <summary>
/// Determinate progress percent for queue row meta — parity with Tauri
/// <c>progressPercent.js</c> (<c>displayProgressPercent</c>).
/// Clamps at 100 so merged-stream bytes over format size never show &gt;100%.
/// WinUI keeps one decimal in the subtitle; Tauri rounds to an integer.
/// </summary>
public static class ProgressDisplay
{
    /// <summary>
    /// Percent 0–100 for display, or null when total unknown / zero.
    /// </summary>
    public static double? DisplayPercent(ulong bytesDownloaded, ulong? totalBytes)
    {
        if (!totalBytes.HasValue || totalBytes.Value == 0)
            return null;

        var percent = (double)bytesDownloaded / totalBytes.Value * 100.0;
        return percent > 100.0 ? 100.0 : percent;
    }
}
