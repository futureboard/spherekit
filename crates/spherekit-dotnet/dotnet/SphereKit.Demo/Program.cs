using System.Globalization;
using SphereKit;

var frameLimit = ParseFrameLimit(args);
var app = new ControlSurface();
app.Run(frameLimit);
Console.WriteLine($"drew {app.Frames} frames");

static ulong? ParseFrameLimit(string[] args)
{
    int index = Array.IndexOf(args, "--frames");
    return index >= 0 && index + 1 < args.Length
        ? ulong.Parse(args[index + 1], CultureInfo.InvariantCulture)
        : null;
}

sealed class ControlSurface : App
{
    private static readonly string[] Tasks =
    {
        "Wire the audio thread",
        "Draw the spectrum",
        "Tune the fader curve",
        "Fix the collapsed sidebar",
        "Write the C# binding",
        "Ship it",
    };

    private int _count;
    private double _volume;
    private double _drive;
    private double _level;
    private bool _muted;
    private bool _showDone;
    private string _name = string.Empty;
    private string _status = string.Empty;
    private readonly HashSet<int> _done = new() { 1 };
    private DateTime _started;

    public ControlSurface()
    {
        Title = "SphereKit — C#/.NET";
        Width = 940;
        Height = 660;
        Css = Stylesheet;
    }

    protected override void Setup()
    {
        _count = 0;
        _volume = 0.65;
        _drive = 0.3;
        _level = 0.8;
        _muted = false;
        _showDone = true;
        _name = "Untitled session";
        _status = "Ready.";
        _started = DateTime.UtcNow;
    }

    public override Node Render()
    {
        double sweep = (DateTime.UtcNow - _started).TotalSeconds % 4 / 4;
        return UI.View("page").Add(
            UI.View("row").Add(
                UI.Avatar("Ada Lovelace", size: 32, presence: "online"),
                UI.View(style: new Dictionary<string, object?> { ["flex"] = "1 1 0" }).Add(
                    UI.Text("C# control surface", className: "title"),
                    UI.Text($"{UI.Version} · frame {Frames}", className: "subtle")),
                UI.Button("Quit", onPress: App.Quit)),
            UI.Separator(),
            UI.View("columns").Add(CounterCard(), MixerCard(sweep), ListCard()),
            UI.View("status").Add(UI.Text(_status)));
    }

    private Node CounterCard() => UI.View("card").Add(
        UI.Text("Counter", className: "subtle"),
        UI.Text(_count, className: "count"),
        UI.View("row").Add(
            UI.Button("Add one", onPress: Add),
            UI.Button("Reset", onPress: Reset, disabled: _count == 0)),
        UI.Separator(),
        UI.Text("Session name", className: "subtle"),
        UI.TextField(_name, placeholder: "Name this session", onChange: Rename, onSubmit: SubmitName),
        UI.View(style: new Dictionary<string, object?> { ["flex"] = "1 1 0" }),
        UI.MenuItem("Duplicate", shortcut: "Ctrl+D", onSelect: () => Say("Duplicated.")),
        UI.MenuItem("Delete", shortcut: "Del", danger: true, onSelect: () => Say("Deleted.")));

    private Node MixerCard(double sweep) => UI.View("card").Add(
        UI.Text("Mixer", className: "subtle"),
        UI.Text($"Volume {_volume:P0}"),
        UI.Slider(_volume, onChange: SetVolume, name: "Volume"),
        UI.Text($"Drive {_drive:F2}"),
        UI.View("row").Add(
            UI.Knob(_drive, onChange: SetDrive, name: "Drive",
                style: new Dictionary<string, object?> { ["width"] = "56px", ["height"] = "56px" }),
            UI.Fader(_level, onChange: SetLevel, name: "Level",
                style: new Dictionary<string, object?> { ["width"] = "36px", ["height"] = "110px" })),
        UI.Toggle(_muted, label: "Mute", onChange: SetMuted),
        UI.Separator(),
        UI.Text("Render", className: "subtle"),
        UI.Progress(sweep));

    private Node ListCard()
    {
        IEnumerable<Node> rows = Tasks
            .Select((task, index) => (task, index))
            .Where(item => _showDone || !_done.Contains(item.index))
            .Select(item => UI.View(
                "list-row",
                key: $"task-{item.index}",
                onPress: () => ToggleTask(item.index)).Add(
                UI.Text(_done.Contains(item.index) ? "done" : "to do", className: "subtle"),
                UI.Text(item.task, className: _done.Contains(item.index) ? "done" : null)));

        return UI.View("card").Add(
            UI.View("row").Add(
                UI.Text("Tasks", className: "subtle"),
                UI.View(style: new Dictionary<string, object?> { ["flex"] = "1 1 0" }),
                UI.Checkbox(_showDone, label: "Show done", onChange: SetShowDone)),
            UI.ScrollView(className: "list").Add(rows));
    }

    private void Add() { _count++; _status = $"Count is {_count}."; }
    private void Reset() { _count = 0; _status = "Count reset."; }
    private void SetVolume(double value) { _volume = value; _status = $"Volume {value:P0}"; }
    private void SetDrive(double value) => _drive = value;
    private void SetLevel(double value) => _level = value;
    private void SetMuted(bool value) { _muted = value; _status = value ? "Muted." : "Unmuted."; }
    private void SetShowDone(bool value) => _showDone = value;
    private void Rename(string value) => _name = value;
    private void SubmitName(string value) => _status = $"Renamed to {value}.";
    private void ToggleTask(int index)
    {
        if (!_done.Add(index)) _done.Remove(index);
        _status = $"{Tasks[index]}: {(_done.Contains(index) ? "done" : "to do")}.";
    }
    private void Say(string message) => _status = message;

    private const string Stylesheet = """
        .page { display: flex; flex-direction: column; width: 100%; height: 100%; padding: 16px; gap: 12px; }
        .title { font-size: 22px; font-weight: 700; color: #f2f3f5; }
        .subtle { font-size: 12px; color: #9aa0a6; }
        .row { display: flex; flex-direction: row; align-items: center; gap: 12px; }
        .columns { display: flex; flex-direction: row; gap: 16px; flex: 1 1 0; min-height: 0; }
        .card { display: flex; flex-direction: column; gap: 10px; padding: 14px; background: #26282c; border-radius: 10px; flex: 1 1 0; min-height: 0; }
        .count { font-size: 40px; font-weight: 700; color: #8ab4f8; }
        .list { display: flex; flex-direction: column; gap: 4px; flex: 1 1 0; min-height: 0; }
        .list-row { display: flex; flex-direction: row; align-items: center; gap: 8px; padding: 6px 10px; border-radius: 6px; background: #2f3236; }
        .list-row:hover { background: #3a3e44; }
        .done { color: #6f7681; }
        .status { display: flex; flex-direction: row; align-items: center; height: 26px; padding: 0 6px; color: #9aa0a6; font-size: 12px; }
        """;
}
