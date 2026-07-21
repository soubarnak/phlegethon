using System;
using System.Diagnostics;
using System.IO;

namespace Phlegethon.Services;

/// <summary>Reveal or open completed job output files on disk.</summary>
public static class OutputFileActions
{
    public static void RevealInFolder(string filePath)
    {
        var fullPath = Path.GetFullPath(filePath);
        if (!File.Exists(fullPath) && !Directory.Exists(fullPath))
        {
            throw new FileNotFoundException("Output file not found.", fullPath);
        }

        Process.Start(new ProcessStartInfo
        {
            FileName = "explorer.exe",
            Arguments = $"/select,\"{fullPath}\"",
            UseShellExecute = true,
        });
    }

    public static void OpenFile(string filePath)
    {
        var fullPath = Path.GetFullPath(filePath);
        if (!File.Exists(fullPath))
        {
            throw new FileNotFoundException("Output file not found.", fullPath);
        }

        Process.Start(new ProcessStartInfo
        {
            FileName = fullPath,
            UseShellExecute = true,
        });
    }
}
