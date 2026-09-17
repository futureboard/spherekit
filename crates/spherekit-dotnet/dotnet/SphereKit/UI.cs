using System.Globalization;

namespace SphereKit;

/// <summary>Node builders and native validation helpers for SphereKit.</summary>
public static class UI
{
    /// <summary>Version of the native SphereKit library.</summary>
    public static string Version => Native.Version;

    /// <summary>The host types lowered by the native widget layer.</summary>
    public static IReadOnlyList<string> NodeTypes => Native.NodeTypes;

    /// <summary>Validates a committed tree and returns its node count.</summary>
    public static long CheckTree(string json) => Native.CheckTree(json);

    /// <summary>Parses a stylesheet and returns its rule count.</summary>
    public static long CheckCss(string css) => Native.CheckCss(css);

    /// <summary>Requests the active window to close.</summary>
    public static void Quit() => Native.spherekit_dotnet_quit();

    /// <summary>Creates a container node.</summary>
    public static Node View(
        string? className = null,
        IReadOnlyDictionary<string, object?>? style = null,
        string? elementId = null,
        object? key = null,
        Action? onPress = null,
        IReadOnlyDictionary<string, object?>? props = null)
    {
        Node node = Create("view", className, style, elementId, key, props);
        if (onPress is not null)
        {
            node.SetProp("pressable", true).On("press", Wrap(onPress));
        }
        return node;
    }

    /// <summary>Creates a text label with a <c>#text</c> child.</summary>
    public static Node Text(
        object? value = null,
        string? className = null,
        IReadOnlyDictionary<string, object?>? style = null,
        string? elementId = null,
        object? key = null,
        IReadOnlyDictionary<string, object?>? props = null)
    {
        Node node = Create("text", className, style, elementId, key, props);
        node.Add(new Node("#text") { Text = Convert.ToString(value ?? string.Empty, CultureInfo.InvariantCulture) });
        return node;
    }

    /// <summary>Creates a push button.</summary>
    public static Node Button(
        string title,
        Action? onPress = null,
        bool disabled = false,
        string? className = null,
        IReadOnlyDictionary<string, object?>? style = null,
        string? elementId = null,
        object? key = null,
        IReadOnlyDictionary<string, object?>? props = null)
    {
        Node node = Create("button", className, style, elementId, key, props);
        node.SetProp("title", title).SetProp("disabled", disabled);
        ApplyExtra(node, props);
        return onPress is null ? node : node.On("press", Wrap(onPress));
    }

    /// <summary>Creates a horizontal value control.</summary>
    public static Node Slider(double value, double minimum = 0, double maximum = 1, double? step = null,
        string? name = null, bool disabled = false, Action<double>? onChange = null,
        string? className = null, IReadOnlyDictionary<string, object?>? style = null,
        string? elementId = null, object? key = null, IReadOnlyDictionary<string, object?>? props = null) =>
        Range("slider", value, minimum, maximum, step, name, disabled, onChange,
            className, style, elementId, key, props);

    /// <summary>Creates a rotary value control.</summary>
    public static Node Knob(double value, double minimum = 0, double maximum = 1, double? step = null,
        string? name = null, bool disabled = false, Action<double>? onChange = null,
        string? className = null, IReadOnlyDictionary<string, object?>? style = null,
        string? elementId = null, object? key = null, IReadOnlyDictionary<string, object?>? props = null) =>
        Range("knob", value, minimum, maximum, step, name, disabled, onChange,
            className, style, elementId, key, props);

    /// <summary>Creates a vertical fader.</summary>
    public static Node Fader(double value, double minimum = 0, double maximum = 1, double? step = null,
        string? name = null, bool disabled = false, Action<double>? onChange = null,
        string? className = null, IReadOnlyDictionary<string, object?>? style = null,
        string? elementId = null, object? key = null, IReadOnlyDictionary<string, object?>? props = null) =>
        Range("fader", value, minimum, maximum, step, name, disabled, onChange,
            className, style, elementId, key, props);

    /// <summary>Creates a switch.</summary>
    public static Node Toggle(bool checkedValue, string? label = null, bool disabled = false,
        Action<bool>? onChange = null, string? className = null,
        IReadOnlyDictionary<string, object?>? style = null, string? elementId = null,
        object? key = null, IReadOnlyDictionary<string, object?>? props = null) =>
        Switch("toggle", checkedValue, label, disabled, onChange,
            className, style, elementId, key, props);

    /// <summary>Creates a checkbox.</summary>
    public static Node Checkbox(bool checkedValue, string? label = null, bool disabled = false,
        Action<bool>? onChange = null, string? className = null,
        IReadOnlyDictionary<string, object?>? style = null, string? elementId = null,
        object? key = null, IReadOnlyDictionary<string, object?>? props = null) =>
        Switch("checkbox", checkedValue, label, disabled, onChange,
            className, style, elementId, key, props);

    /// <summary>Creates a single-line editable field.</summary>
    public static Node TextField(string value = "", string? placeholder = null, bool mask = false,
        bool disabled = false, Action<string>? onChange = null, Action<string>? onSubmit = null,
        string? className = null, IReadOnlyDictionary<string, object?>? style = null,
        string? elementId = null, object? key = null, IReadOnlyDictionary<string, object?>? props = null)
    {
        Node node = Create("text-field", className, style, elementId, key, props);
        node.SetProp("value", value).SetOptionalProp("placeholder", placeholder).SetProp("mask", mask)
            .SetProp("disabled", disabled);
        ApplyExtra(node, props);
        if (onChange is not null) node.On("change", Wrap(onChange));
        if (onSubmit is not null) node.On("submit", Wrap(onSubmit));
        return node;
    }

    /// <summary>Creates a progress bar.</summary>
    public static Node Progress(double value = 0, bool indeterminate = false, double? thickness = null,
        string? className = null, IReadOnlyDictionary<string, object?>? style = null,
        string? elementId = null, object? key = null, IReadOnlyDictionary<string, object?>? props = null)
    {
        Node node = Create("progress", className, style, elementId, key, props)
            .SetProp("value", value).SetProp("indeterminate", indeterminate)
            .SetOptionalProp("thickness", thickness);
        return ApplyExtra(node, props);
    }

    /// <summary>Creates a divider.</summary>
    public static Node Separator(bool vertical = false, string? className = null,
        IReadOnlyDictionary<string, object?>? style = null, string? elementId = null,
        object? key = null, IReadOnlyDictionary<string, object?>? props = null) =>
        ApplyExtra(Create("separator", className, style, elementId, key, props)
            .SetProp("vertical", vertical), props);

    /// <summary>Creates a titled group box.</summary>
    public static Node Panel(string title = "", string? className = null,
        IReadOnlyDictionary<string, object?>? style = null, string? elementId = null,
        object? key = null, IReadOnlyDictionary<string, object?>? props = null) =>
        ApplyExtra(Create("panel", className, style, elementId, key, props)
            .SetProp("title", title), props);

    /// <summary>Creates a scrolling container.</summary>
    public static Node ScrollView(bool horizontal = false, bool both = false, string? className = null,
        IReadOnlyDictionary<string, object?>? style = null, string? elementId = null,
        object? key = null, IReadOnlyDictionary<string, object?>? props = null) =>
        ApplyExtra(Create("scroll-view", className, style, elementId, key, props)
            .SetProp("horizontal", horizontal).SetProp("both", both), props);

    /// <summary>Creates an avatar.</summary>
    public static Node Avatar(string name, string? initials = null, double? size = null,
        string? presence = null, string? className = null,
        IReadOnlyDictionary<string, object?>? style = null, string? elementId = null,
        object? key = null, IReadOnlyDictionary<string, object?>? props = null) =>
        ApplyExtra(Create("avatar", className, style, elementId, key, props)
            .SetProp("name", name).SetOptionalProp("initials", initials).SetOptionalProp("size", size)
            .SetOptionalProp("presence", presence), props);

    /// <summary>Creates one menu row.</summary>
    public static Node MenuItem(string label, string? shortcut = null, bool danger = false,
        bool disabled = false, Action? onSelect = null, string? className = null,
        IReadOnlyDictionary<string, object?>? style = null, string? elementId = null,
        object? key = null, IReadOnlyDictionary<string, object?>? props = null)
    {
        Node node = Create("menu-item", className, style, elementId, key, props)
            .SetProp("label", label).SetOptionalProp("shortcut", shortcut)
            .SetProp("danger", danger).SetProp("disabled", disabled);
        ApplyExtra(node, props);
        return onSelect is null ? node : node.On("select", Wrap(onSelect));
    }

    private static Node Create(string type, string? className, IReadOnlyDictionary<string, object?>? style,
        string? elementId, object? key, IReadOnlyDictionary<string, object?>? extra)
    {
        var props = new Dictionary<string, object?>(StringComparer.Ordinal);
        if (!string.IsNullOrEmpty(className)) props["className"] = className;
        if (!string.IsNullOrEmpty(elementId)) props["id"] = elementId;
        if (style is not null && style.Count > 0) props["style"] = new Dictionary<string, object?>(style);
        if (extra is not null)
        {
            foreach (KeyValuePair<string, object?> entry in extra)
            {
                if (entry.Value is not null) props[entry.Key] = entry.Value;
            }
        }
        return new Node(type, props, key);
    }

    private static Node Range(string type, double value, double minimum, double maximum, double? step,
        string? name, bool disabled, Action<double>? onChange, string? className,
        IReadOnlyDictionary<string, object?>? style, string? elementId, object? key,
        IReadOnlyDictionary<string, object?>? props)
    {
        Node node = Create(type, className, style, elementId, key, props)
            .SetProp("value", value).SetProp("minimumValue", minimum).SetProp("maximumValue", maximum)
            .SetOptionalProp("step", step).SetOptionalProp("name", name).SetProp("disabled", disabled);
        ApplyExtra(node, props);
        return onChange is null ? node : node.On("valueChange", Wrap(onChange));
    }

    private static Node Switch(string type, bool checkedValue, string? label, bool disabled,
        Action<bool>? onChange, string? className, IReadOnlyDictionary<string, object?>? style,
        string? elementId, object? key, IReadOnlyDictionary<string, object?>? props)
    {
        Node node = Create(type, className, style, elementId, key, props)
            .SetProp("checked", checkedValue).SetOptionalProp("label", label).SetProp("disabled", disabled);
        ApplyExtra(node, props);
        return onChange is null ? node : node.On("change", Wrap(onChange));
    }

    private static Node ApplyExtra(Node node, IReadOnlyDictionary<string, object?>? extra)
    {
        if (extra is not null)
        {
            foreach (KeyValuePair<string, object?> entry in extra)
            {
                if (entry.Value is not null) node.SetProp(entry.Key, entry.Value);
            }
        }
        return node;
    }

    private static Action<object?> Wrap(Action action) => _ => action();
    private static Action<object?> Wrap(Action<string> action) => value => action(Convert.ToString(value, CultureInfo.InvariantCulture) ?? string.Empty);
    private static Action<object?> Wrap(Action<bool> action) => value => action(Convert.ToBoolean(value, CultureInfo.InvariantCulture));
    private static Action<object?> Wrap(Action<double> action) => value => action(Convert.ToDouble(value, CultureInfo.InvariantCulture));
}
