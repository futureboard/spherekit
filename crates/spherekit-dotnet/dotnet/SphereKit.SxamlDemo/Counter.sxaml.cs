using System.Globalization;
using SphereKit;

/// <summary>Code-behind for <c>Counter.sxaml</c>.</summary>
sealed class Counter : App
{
    private static readonly string MarkupTemplate = LoadMarkupTemplate();
    private int _count;

    public Counter()
    {
        Title = "SphereKit — SXAML demo";
        Width = 560;
        Height = 420;
        Css = Stylesheet;
    }

    public override Node Render()
    {
        // SXAML stays in a normal file. These two substitutions are ordinary
        // C# template data; the parsed result is still the regular Node tree.
        string markup = MarkupTemplate
            .Replace("{{COUNT}}", _count.ToString(CultureInfo.InvariantCulture), StringComparison.Ordinal)
            .Replace("{{RESET_DISABLED}}", (_count == 0).ToString(), StringComparison.Ordinal);

        return Sxaml.Load(markup, new SxamlContext()
            .On("Add", Add)
            .On("Reset", Reset)
            .On("Quit", App.Quit));
    }

    private void Add() => _count++;

    private void Reset() => _count = 0;

    private static string LoadMarkupTemplate()
    {
        System.Reflection.Assembly assembly = typeof(Counter).Assembly;
        string resourceName = assembly.GetManifestResourceNames()
            .Single(name => name.EndsWith(".Counter.sxaml", StringComparison.OrdinalIgnoreCase));
        using Stream stream = assembly.GetManifestResourceStream(resourceName)
            ?? throw new InvalidOperationException($"Embedded resource '{resourceName}' was not found.");
        using var reader = new StreamReader(stream);
        return reader.ReadToEnd();
    }

    private const string Stylesheet = """
        .page { display: flex; flex-direction: column; gap: 16px; padding: 24px; height: 100%; }
        .title { font-size: 26px; font-weight: 700; color: #f2f3f5; }
        .subtle { font-size: 13px; color: #9aa0a6; }
        .card { display: flex; flex-direction: column; gap: 12px; padding: 18px; background: #26282c; border-radius: 10px; }
        .count { font-size: 64px; font-weight: 700; color: #8ab4f8; }
        .row { display: flex; flex-direction: row; gap: 10px; }
        """;
}
