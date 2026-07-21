using System;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.Toolkit.Uwp.Notifications;

namespace Phlegethon.Services;

/// <summary>Desktop toast notifications for job Completed / Failed events.</summary>
public sealed class DesktopNotificationService
{
    private const string JobIdArgument = "jobId";
    private readonly DispatcherQueue _dispatcher;
    private readonly Action<string> _onJobFocusRequested;
    private static bool _activatedHandlerRegistered;

    public DesktopNotificationService(DispatcherQueue dispatcher, Action<string> onJobFocusRequested)
    {
        _dispatcher = dispatcher;
        _onJobFocusRequested = onJobFocusRequested;
    }

    public void EnsureRegistered()
    {
        if (_activatedHandlerRegistered)
        {
            return;
        }

        ToastNotificationManagerCompat.OnActivated += OnToastActivated;
        _activatedHandlerRegistered = true;
    }

    public void ShowJobNotification(string jobId, string title, string body)
    {
        EnsureRegistered();

        new ToastContentBuilder()
            .AddArgument(JobIdArgument, jobId)
            .AddText(title)
            .AddText(body)
            .Show();
    }

    private void OnToastActivated(ToastNotificationActivatedEventArgsCompat args)
    {
        var jobId = ToastArguments.Parse(args.Argument).Get(JobIdArgument);
        if (string.IsNullOrWhiteSpace(jobId))
        {
            return;
        }

        _dispatcher.TryEnqueue(() =>
        {
            if (Application.Current is App app && app.MainWindow != null)
            {
                app.MainWindow.Activate();
            }

            _onJobFocusRequested(jobId);
        });
    }
}
