using System.Globalization;
using System.Reflection;
using System.Xml;
using System.Xml.Linq;

namespace SphereKit;

/// <summary>
/// Named callbacks used by SXAML event attributes.
/// </summary>
/// <remarks>
/// SXAML is intentionally a small XML-to-<see cref="Node"/> layer. It does
/// not introduce another renderer or widget set: the resulting tree is sent
/// through the same serializer and native host as the C# builders.
/// </remarks>
public sealed class SxamlContext
{
    private readonly Dictionary<string, Action<object?>> _handlers = new(StringComparer.Ordinal);

    /// <summary>Registers a callback which does not use the event payload.</summary>
    public SxamlContext On(string handlerName, Action handler)
    {
        ArgumentNullException.ThrowIfNull(handler);
        return Register(handlerName, handler);
    }

    /// <summary>Registers a callback which receives a numeric event payload.</summary>
    public SxamlContext On(string handlerName, Action<double> handler)
    {
        ArgumentNullException.ThrowIfNull(handler);
        return Register(handlerName, handler);
    }

    /// <summary>Registers a callback which receives a boolean event payload.</summary>
    public SxamlContext On(string handlerName, Action<bool> handler)
    {
        ArgumentNullException.ThrowIfNull(handler);
        return Register(handlerName, handler);
    }

    /// <summary>Registers a callback which receives a string event payload.</summary>
    public SxamlContext On(string handlerName, Action<string> handler)
    {
        ArgumentNullException.ThrowIfNull(handler);
        return Register(handlerName, handler);
    }

    /// <summary>Registers a callback which receives the raw event payload.</summary>
    public SxamlContext On(string handlerName, Action<object?> handler)
    {
        ArgumentNullException.ThrowIfNull(handler);
        return Register(handlerName, handler);
    }

    /// <summary>Registers a compatible one-argument or zero-argument delegate.</summary>
    public SxamlContext On(string handlerName, Delegate handler)
    {
        ArgumentNullException.ThrowIfNull(handler);
        return Register(handlerName, handler);
    }

    internal bool TryGet(string handlerName, out Action<object?> handler)
    {
        return _handlers.TryGetValue(handlerName, out handler!);
    }

    private SxamlContext Register(string handlerName, Delegate handler)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(handlerName);
        _handlers[handlerName] = Adapt(handler);
        return this;
    }

    private static Action<object?> Adapt(Delegate handler)
    {
        ParameterInfo[] parameters = handler.Method.GetParameters();
        return parameters.Length switch
        {
            0 => _ => handler.DynamicInvoke(),
            1 => value => handler.DynamicInvoke(ConvertValue(value, parameters[0].ParameterType)),
            _ => throw new ArgumentException(
                "An SXAML handler must have zero or one parameter.", nameof(handler)),
        };
    }

    private static object? ConvertValue(object? value, Type targetType)
    {
        Type destination = Nullable.GetUnderlyingType(targetType) ?? targetType;
        if (value is null)
        {
            return destination.IsValueType ? Activator.CreateInstance(destination) : null;
        }
        if (destination.IsInstanceOfType(value))
        {
            return value;
        }
        if (destination == typeof(string))
        {
            return Convert.ToString(value, CultureInfo.InvariantCulture) ?? string.Empty;
        }
        if (destination.IsEnum)
        {
            return Enum.Parse(destination, Convert.ToString(value, CultureInfo.InvariantCulture)!, true);
        }
        return Convert.ChangeType(value, destination, CultureInfo.InvariantCulture);
    }
}

/// <summary>Parses SphereKit XML (SXAML) into the normal retained node tree.</summary>
public static class Sxaml
{
    private const string XamlNamespace = "http://schemas.microsoft.com/winfx/2006/xaml";
    private const string XmlnsNamespace = "http://www.w3.org/2000/xmlns/";

    private static readonly HashSet<string> BooleanProps = new(StringComparer.Ordinal)
    {
        "disabled", "checked", "mask", "indeterminate", "vertical", "horizontal",
        "both", "danger", "pressable",
    };

    private static readonly HashSet<string> NumericProps = new(StringComparer.Ordinal)
    {
        "value", "minimumValue", "maximumValue", "step", "thickness", "size",
    };

    /// <summary>Parses one SXAML root element.</summary>
    /// <exception cref="SxamlException">The document is invalid or has no single root node.</exception>
    public static Node Load(string markup, SxamlContext? context = null)
    {
        IReadOnlyList<Node> roots = LoadMany(markup, context);
        if (roots.Count != 1)
        {
            throw new SxamlException(
                $"SXAML must contain exactly one root node; found {roots.Count}.");
        }
        return roots[0];
    }

    /// <summary>Parses a file containing one SXAML root element.</summary>
    public static Node LoadFile(string path, SxamlContext? context = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        try
        {
            return Load(File.ReadAllText(path), context);
        }
        catch (SxamlException)
        {
            throw;
        }
        catch (Exception exception) when (exception is IOException or UnauthorizedAccessException)
        {
            throw new SxamlException($"Could not read SXAML file '{path}'.", exception);
        }
    }

    /// <summary>
    /// Parses SXAML. A <c>Sxaml</c> or <c>Fragment</c> root can contain several
    /// sibling nodes; any other root returns a one-item list.
    /// </summary>
    public static IReadOnlyList<Node> LoadMany(string markup, SxamlContext? context = null)
    {
        ArgumentNullException.ThrowIfNull(markup);
        XDocument document;
        try
        {
            document = XDocument.Parse(markup, LoadOptions.PreserveWhitespace | LoadOptions.SetLineInfo);
        }
        catch (Exception exception) when (exception is XmlException or InvalidOperationException)
        {
            throw new SxamlException("SXAML XML is not well formed.", exception);
        }

        XElement root = document.Root
            ?? throw new SxamlException("SXAML does not contain a root element.");
        IEnumerable<XElement> elements = IsFragment(root)
            ? root.Elements()
            : new[] { root };
        var output = new List<Node>();
        foreach (XElement element in elements)
        {
            output.Add(ParseElement(element, context));
        }
        return output;
    }

    private static Node ParseElement(XElement element, SxamlContext? context)
    {
        string sourceName = element.Name.LocalName;
        string type = ResolveType(sourceName, element);
        var props = new Dictionary<string, object?>(StringComparer.Ordinal);
        object? key = null;
        string? content = null;
        var eventBindings = new List<(string EventName, string HandlerName)>();

        ApplyLayoutAliasDefaults(sourceName, props);
        foreach (XAttribute attribute in element.Attributes())
        {
            if (attribute.IsNamespaceDeclaration || attribute.Name.NamespaceName == XmlnsNamespace)
            {
                continue;
            }

            string localName = attribute.Name.LocalName;
            if (attribute.Name.NamespaceName == XamlNamespace)
            {
                if (localName.Equals("Name", StringComparison.OrdinalIgnoreCase))
                {
                    props["id"] = attribute.Value;
                    continue;
                }
                if (localName.Equals("Key", StringComparison.OrdinalIgnoreCase))
                {
                    key = ParseScalar(attribute.Value);
                    continue;
                }
                throw Error(element, $"Unsupported x: attribute '{localName}'.");
            }
            if (attribute.Name.NamespaceName == XNamespace.Xml.NamespaceName)
            {
                continue;
            }
            if (TryGetEventName(localName, out string eventName))
            {
                eventBindings.Add((eventName, attribute.Value.Trim()));
                continue;
            }

            string propName = ToPropName(localName);
            if (propName == "key")
            {
                key = ParseScalar(attribute.Value);
                continue;
            }
            if (propName == "style")
            {
                foreach (KeyValuePair<string, object?> entry in ParseStyle(attribute.Value, element))
                {
                    props.TryGetValue("style", out object? existing);
                    if (existing is not Dictionary<string, object?> style)
                    {
                        style = new Dictionary<string, object?>(StringComparer.Ordinal);
                        props["style"] = style;
                    }
                    style[entry.Key] = entry.Value;
                }
                continue;
            }
            if (propName == "className")
            {
                props[propName] = attribute.Value;
                continue;
            }
            if (propName == "id")
            {
                props[propName] = attribute.Value;
                continue;
            }
            if (propName == "content" || type == "text" && propName == "text")
            {
                content = attribute.Value;
                continue;
            }
            props[propName] = ParsePropertyValue(propName, attribute.Value);
        }

        string text = DirectText(element);
        if (!string.IsNullOrWhiteSpace(text))
        {
            content = text.Trim();
        }

        ApplyContent(type, content, props);
        Node node = new(type, props, key);
        if (type == "text" && content is not null)
        {
            node.Add(new Node("#text") { Text = content });
        }
        else if (type is not ("button" or "menu-item" or "panel") && !string.IsNullOrWhiteSpace(text))
        {
            node.Add(text.Trim());
        }

        foreach (XElement child in element.Elements())
        {
            node.Add(ParseElement(child, context));
        }

        foreach ((string eventName, string handlerName) in eventBindings)
        {
            if (handlerName.Length == 0)
            {
                throw Error(element, $"SXAML event '{eventName}' has an empty handler name.");
            }
            if (context is null || !context.TryGet(handlerName, out Action<object?>? handler))
            {
                throw Error(element,
                    $"SXAML handler '{handlerName}' for event '{eventName}' was not registered.");
            }
            node.On(eventName, handler);
        }
        return node;
    }

    private static string ResolveType(string sourceName, XElement element)
    {
        return sourceName.ToLowerInvariant() switch
        {
            "view" or "stackpanel" or "verticalstack" or "horizontalstack" or "grid" => "view",
            "text" or "label" or "textblock" => "text",
            "button" => "button",
            "slider" => "slider",
            "knob" => "knob",
            "fader" => "fader",
            "toggle" => "toggle",
            "checkbox" => "checkbox",
            "textfield" or "text-field" => "text-field",
            "progress" => "progress",
            "separator" or "divider" => "separator",
            "panel" => "panel",
            "scrollview" or "scroll-view" => "scroll-view",
            "avatar" => "avatar",
            "menuitem" or "menu-item" => "menu-item",
            _ => throw Error(element, $"Unsupported SXAML element '{sourceName}'."),
        };
    }

    private static void ApplyLayoutAliasDefaults(string sourceName, IDictionary<string, object?> props)
    {
        string name = sourceName.ToLowerInvariant();
        if (name is "stackpanel" or "verticalstack")
        {
            props["style"] = new Dictionary<string, object?>(StringComparer.Ordinal)
            {
                ["display"] = "flex",
                ["flex-direction"] = "column",
            };
        }
        else if (name == "horizontalstack")
        {
            props["style"] = new Dictionary<string, object?>(StringComparer.Ordinal)
            {
                ["display"] = "flex",
                ["flex-direction"] = "row",
            };
        }
        else if (name == "grid")
        {
            props["style"] = new Dictionary<string, object?>(StringComparer.Ordinal)
            {
                ["display"] = "flex",
                ["flex-direction"] = "column",
            };
        }
    }

    private static void ApplyContent(string type, string? content, IDictionary<string, object?> props)
    {
        if (content is null)
        {
            return;
        }
        switch (type)
        {
            case "button":
                props.TryAdd("title", content);
                break;
            case "menu-item":
                props.TryAdd("label", content);
                break;
            case "panel":
                props.TryAdd("title", content);
                break;
        }
    }

    private static string DirectText(XElement element)
    {
        return string.Concat(element.Nodes().OfType<XText>().Select(text => text.Value));
    }

    private static Dictionary<string, object?> ParseStyle(string value, XElement element)
    {
        var style = new Dictionary<string, object?>(StringComparer.Ordinal);
        foreach (string part in value.Split(';', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            int separator = part.IndexOf(':');
            if (separator <= 0)
            {
                throw Error(element, $"SXAML style entry '{part}' must use 'property: value'.");
            }
            string name = part[..separator].Trim();
            string propertyValue = part[(separator + 1)..].Trim();
            if (name.Length == 0 || propertyValue.Length == 0)
            {
                throw Error(element, $"SXAML style entry '{part}' is incomplete.");
            }
            style[name] = propertyValue;
        }
        return style;
    }

    private static object? ParsePropertyValue(string propName, string value)
    {
        string trimmed = value.Trim();
        if (BooleanProps.Contains(propName) && bool.TryParse(trimmed, out bool boolean))
        {
            return boolean;
        }
        if (NumericProps.Contains(propName)
            && double.TryParse(trimmed, NumberStyles.Float, CultureInfo.InvariantCulture, out double number))
        {
            return number;
        }
        return ParseScalar(value);
    }

    private static object ParseScalar(string value)
    {
        string trimmed = value.Trim();
        if (bool.TryParse(trimmed, out bool boolean))
        {
            return boolean;
        }
        if (long.TryParse(trimmed, NumberStyles.Integer, CultureInfo.InvariantCulture, out long integer))
        {
            return integer;
        }
        if (double.TryParse(trimmed, NumberStyles.Float, CultureInfo.InvariantCulture, out double number))
        {
            return number;
        }
        return value;
    }

    private static string ToPropName(string name)
    {
        string normalized = name.Replace("-", string.Empty, StringComparison.Ordinal);
        return normalized switch
        {
            "Class" or "class" or "ClassName" or "classname" => "className",
            "Id" or "id" => "id",
            "Minimum" or "minimum" => "minimumValue",
            "Maximum" or "maximum" => "maximumValue",
            _ when normalized.Length == 0 => throw new ArgumentException("An SXAML property name is empty."),
            _ when char.IsLower(normalized[0]) => normalized,
            _ => char.ToLowerInvariant(normalized[0]) + normalized[1..],
        };
    }

    private static bool TryGetEventName(string attributeName, out string eventName)
    {
        string compact = new(attributeName.Where(char.IsLetterOrDigit).ToArray());
        if (compact.StartsWith("on", StringComparison.OrdinalIgnoreCase))
        {
            compact = compact[2..];
        }
        eventName = compact.ToLowerInvariant() switch
        {
            "press" or "click" => "press",
            "valuechange" => "valueChange",
            "change" => "change",
            "submit" => "submit",
            "select" => "select",
            _ => string.Empty,
        };
        return eventName.Length > 0;
    }

    private static bool IsFragment(XElement element)
    {
        return element.Name.NamespaceName.Length == 0
            && (element.Name.LocalName.Equals("Sxaml", StringComparison.OrdinalIgnoreCase)
                || element.Name.LocalName.Equals("Fragment", StringComparison.OrdinalIgnoreCase));
    }

    private static SxamlException Error(XElement element, string message)
    {
        if (element is IXmlLineInfo info && info.HasLineInfo())
        {
            message += $" (line {info.LineNumber}, position {info.LinePosition})";
        }
        return new SxamlException(message);
    }
}
