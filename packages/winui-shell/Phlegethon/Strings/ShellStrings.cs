using System.Reflection;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace Phlegethon.Strings;

/// <summary>
/// Locale-aware string lookup. English is the shipped locale; additional locales
/// can be added by embedding more JSON tables without restructuring callers.
/// </summary>
public static class ShellStrings
{
    private static readonly Dictionary<string, string> Table = LoadTable("en");

    public static string CurrentLocale => "en";

    public static string Get(string key) =>
        Table.TryGetValue(key, out var value) ? value : key;

    public static string Format(string key, IReadOnlyDictionary<string, string>? args)
    {
        var template = Get(key);
        if (args is null || args.Count == 0)
        {
            return template;
        }

        return Regex.Replace(template, @"\{(\w+)\}", match =>
        {
            var name = match.Groups[1].Value;
            return args.TryGetValue(name, out var value) ? value : match.Value;
        });
    }

    // Static accessors for XAML x:Bind in data templates.
    public static string QueueTitle => Get("queue.title");
    public static string QueueActionMoveUp => Get("queue.action.moveUp");
    public static string QueueActionMoveDown => Get("queue.action.moveDown");
    public static string QueueActionDiscard => Get("queue.action.discard");
    public static string QueueActionReveal => Get("queue.action.reveal");
    public static string QueueActionOpen => Get("queue.action.open");

    private static Dictionary<string, string> LoadTable(string locale)
    {
        var assembly = Assembly.GetExecutingAssembly();
        var resourceName = $"Phlegethon.Strings.{locale}.json";
        using var stream = assembly.GetManifestResourceStream(resourceName)
            ?? throw new InvalidOperationException($"Missing embedded strings: {resourceName}");
        using var reader = new StreamReader(stream);
        var json = reader.ReadToEnd();
        return JsonSerializer.Deserialize<Dictionary<string, string>>(json)
            ?? throw new InvalidOperationException($"Invalid strings JSON: {resourceName}");
    }
}
