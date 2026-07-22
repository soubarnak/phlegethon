using System;
using System.IO;
using Microsoft.UI.Xaml;

namespace Phlegethon;

public partial class App : Application
{
    private MainWindow? _window;

    /// <summary>
    /// The active main window (set on launch).
    /// </summary>
    public MainWindow? MainWindow => _window;

    /// <summary>
    /// Initializes the singleton application object.
    /// </summary>
    public App()
    {
        UnhandledException += OnUnhandledException;
        try
        {
            this.InitializeComponent();
        }
        catch (Exception ex)
        {
            WriteStartupCrash("InitializeComponent", ex);
            throw;
        }
    }

    /// <summary>
    /// Invoked when the application is launched.
    /// Creates the MainWindow and shows it.
    /// </summary>
    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        try
        {
            _window = new MainWindow();
            _window.Activate();
        }
        catch (Exception ex)
        {
            WriteStartupCrash("OnLaunched", ex);
            throw;
        }
    }

    private void OnUnhandledException(object sender, Microsoft.UI.Xaml.UnhandledExceptionEventArgs e)
    {
        WriteStartupCrash("UnhandledException", e.Exception);
        e.Handled = true;
    }

    private static void WriteStartupCrash(string stage, Exception ex)
    {
        try
        {
            var path = Path.Combine(AppContext.BaseDirectory, "startup-crash.log");
            File.WriteAllText(path, $"[{DateTime.Now:O}] {stage}\n{ex}\n");
        }
        catch
        {
            // ignore logging failures
        }
    }
}
