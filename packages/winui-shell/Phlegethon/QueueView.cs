using System;
using System.Collections.Generic;
using System.Linq;
using Phlegethon.Controls;

namespace Phlegethon;

/// <summary>
/// Queue search / filter / sort — view overlay over Core queue identity.
/// Parity with Tauri <c>queueView.js</c>.
/// </summary>
public static class QueueView
{
    public const string FilterAll = "all";

    public const string SortQueue = "queue";
    public const string SortTitle = "title";
    public const string SortStatus = "status";
    public const string SortSize = "size";

    /// <summary>
    /// Case-insensitive substring match on title, URL, destination, and output path.
    /// </summary>
    public static bool MatchesSearch(DownloadItemViewModel job, string? query)
    {
        var q = (query ?? string.Empty).Trim();
        if (q.Length == 0)
        {
            return true;
        }

        return ContainsIgnoreCase(job.Title, q)
            || ContainsIgnoreCase(job.Url, q)
            || ContainsIgnoreCase(job.Destination, q)
            || ContainsIgnoreCase(job.OutputPath, q);
    }

    /// <summary>
    /// Filter by UI status. <c>queued</c> / <c>pending</c> match <see cref="DownloadStatus.Queued"/>.
    /// </summary>
    public static bool MatchesStateFilter(DownloadItemViewModel job, string? stateFilter)
    {
        var filter = (stateFilter ?? FilterAll).Trim().ToLowerInvariant();
        if (filter.Length == 0 || filter == FilterAll)
        {
            return true;
        }

        return filter switch
        {
            "queued" or "pending" => job.Status == DownloadStatus.Queued,
            "downloading" => job.Status == DownloadStatus.Downloading,
            "paused" => job.Status == DownloadStatus.Paused,
            "completed" => job.Status == DownloadStatus.Completed,
            "failed" => job.Status == DownloadStatus.Failed,
            _ => true,
        };
    }

    /// <summary>
    /// Filter + sort a copy. Does not mutate <paramref name="jobs"/> (Core queue identity).
    /// </summary>
    public static IReadOnlyList<DownloadItemViewModel> Apply(
        IReadOnlyList<DownloadItemViewModel> jobs,
        string? search = null,
        string? stateFilter = null,
        string? sort = null)
    {
        var filtered = jobs
            .Where(j => MatchesSearch(j, search) && MatchesStateFilter(j, stateFilter))
            .ToList();

        var sortKey = (sort ?? SortQueue).Trim().ToLowerInvariant();
        if (sortKey == SortQueue || sortKey.Length == 0)
        {
            return filtered;
        }

        return sortKey switch
        {
            SortTitle => filtered
                .OrderBy(j => j.Title ?? string.Empty, StringComparer.OrdinalIgnoreCase)
                .ThenBy(j => j.JobId, StringComparer.Ordinal)
                .ToList(),
            SortStatus => filtered
                .OrderBy(StatusOrder)
                .ThenBy(j => j.Title ?? string.Empty, StringComparer.OrdinalIgnoreCase)
                .ToList(),
            SortSize => filtered
                .OrderByDescending(j => j.TotalBytes.HasValue)
                .ThenByDescending(j => j.TotalBytes ?? 0UL)
                .ThenBy(j => j.JobId, StringComparer.Ordinal)
                .ToList(),
            _ => filtered,
        };
    }

    private static int StatusOrder(DownloadItemViewModel job) => job.Status switch
    {
        DownloadStatus.Completed => 0,
        DownloadStatus.Downloading => 1,
        DownloadStatus.Failed => 2,
        DownloadStatus.Paused => 3,
        DownloadStatus.Queued => 4,
        _ => 99,
    };

    private static bool ContainsIgnoreCase(string? haystack, string needle)
    {
        return !string.IsNullOrEmpty(haystack)
            && haystack.Contains(needle, StringComparison.OrdinalIgnoreCase);
    }
}
