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
        const string title = "Discard job?";
        const string confirmLabel = "Discard";
        const string cancelLabel = "Cancel";
        var deletesFiles = DeletesFiles(status);

        if (!deletesFiles)
        {
            return new Dialog(
                title,
                "This removes the job from the queue only. Files on disk will not be deleted.",
                confirmLabel,
                cancelLabel,
                deletesFiles);
        }

        var path = (destination ?? "").Trim();
        var pathLine = path.Length > 0
            ? $"Files under the Job Destination will be deleted:\n\n{path}"
            : "Files under the Job Destination will be deleted.";

        return new Dialog(
            title,
            $"{pathLine}\n\nThis cannot be undone.",
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
