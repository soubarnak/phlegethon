using System;
using System.Collections.Generic;
using Phlegethon.Strings;

namespace Phlegethon;

/// <summary>
/// Discard confirmation copy — parity with Tauri <c>discardConfirm.js</c>.
/// Completed: queue-row removal only. Otherwise: delete Job Destination files.
/// </summary>
public static class DiscardConfirmCopy
{
    public readonly record struct Dialog(
        string Title,
        string Body,
        string ConfirmLabel,
        string CancelLabel,
        bool DeletesFiles);

    public static bool DeletesFiles(string status) =>
        !string.Equals(status, "completed", StringComparison.OrdinalIgnoreCase);

    public static bool DeletesFiles(Controls.DownloadStatus status) =>
        status != Controls.DownloadStatus.Completed;

    public static Dialog Build(string status, string? destination = null)
    {
        var title = ShellStrings.Get("discard.title");
        var confirmLabel = ShellStrings.Get("discard.confirm");
        var cancelLabel = ShellStrings.Get("discard.cancel");
        var deletesFiles = DeletesFiles(status);

        if (!deletesFiles)
        {
            return new Dialog(
                title,
                ShellStrings.Get("discard.completedBody"),
                confirmLabel,
                cancelLabel,
                deletesFiles);
        }

        var path = (destination ?? "").Trim();
        var pathLine = path.Length > 0
            ? ShellStrings.Format("discard.unfinishedBodyWithPath", new Dictionary<string, string> { ["path"] = path })
            : ShellStrings.Get("discard.unfinishedBody");

        return new Dialog(
            title,
            $"{pathLine}\n\n{ShellStrings.Get("discard.cannotUndo")}",
            confirmLabel,
            cancelLabel,
            deletesFiles);
    }

    public static Dialog Build(Controls.DownloadStatus status, string? destination = null) =>
        Build(StatusToWireName(status), destination);

    private static string StatusToWireName(Controls.DownloadStatus status) => status switch
    {
        Controls.DownloadStatus.Queued => "queued",
        Controls.DownloadStatus.Downloading => "downloading",
        Controls.DownloadStatus.Paused => "paused",
        Controls.DownloadStatus.Completed => "completed",
        Controls.DownloadStatus.Failed => "failed",
        _ => "pending",
    };
}
