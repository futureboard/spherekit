using System.Globalization;

static ulong? ParseFrameLimit(string[] args)
{
    int index = Array.IndexOf(args, "--frames");
    return index >= 0 && index + 1 < args.Length
        ? ulong.Parse(args[index + 1], CultureInfo.InvariantCulture)
        : null;
}

var frameLimit = ParseFrameLimit(args);
var app = new Counter();
app.Run(frameLimit);
Console.WriteLine($"drew {app.Frames} frames");
