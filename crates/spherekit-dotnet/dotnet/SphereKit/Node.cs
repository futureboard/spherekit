using System.Collections;
using System.Globalization;

namespace SphereKit;

/// <summary>A serializable SphereKit host node.</summary>
public sealed class Node
{
    private readonly Dictionary<string, Action<object?>> _handlers = new(StringComparer.Ordinal);

    /// <summary>Creates a host node with an optional set of props.</summary>
    public Node(
        string type,
        IReadOnlyDictionary<string, object?>? props = null,
        object? key = null)
    {
        if (string.IsNullOrWhiteSpace(type))
        {
            throw new ArgumentException("A node type is required.", nameof(type));
        }
        Type = type;
        Key = key;
        Props = props is null
            ? new Dictionary<string, object?>(StringComparer.Ordinal)
            : new Dictionary<string, object?>(props, StringComparer.Ordinal);
    }

    /// <summary>The native host type, such as <c>view</c> or <c>button</c>.</summary>
    public string Type { get; }

    /// <summary>Props sent to the native host.</summary>
    public Dictionary<string, object?> Props { get; }

    /// <summary>Text carried by a <c>#text</c> node.</summary>
    public string? Text { get; internal set; }

    /// <summary>The stable list key, when one was supplied.</summary>
    public object? Key { get; }

    /// <summary>Children in paint order.</summary>
    public List<Node> Children { get; } = new();

    /// <summary>Appends nodes, strings, nulls, or nested enumerables.</summary>
    public Node Add(params object?[] children)
    {
        foreach (object? child in children)
        {
            Flatten(child, Children);
        }
        return this;
    }

    /// <summary>Sets one arbitrary native prop and returns this node.</summary>
    public Node SetProp(string name, object? value)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(name);
        Props[name] = value;
        return this;
    }

    /// <summary>
    /// Serializes this node as a native tree snapshot without a revision.
    /// </summary>
    /// <remarks>
    /// <see cref="App.Run"/> stamps revisions on the native side. This method
    /// is intended for diagnostics, logging, and tests.
    /// </remarks>
    public string ToJson()
    {
        return NodeSerializer.Serialize(new[] { this }, out _);
    }

    internal Node On(string eventName, Action<object?> handler)
    {
        _handlers[eventName] = handler;
        return this;
    }

    internal Node SetOptionalProp(string name, object? value)
    {
        if (value is not null)
        {
            Props[name] = value;
        }
        return this;
    }

    internal IReadOnlyDictionary<string, Action<object?>> Handlers => _handlers;

    internal string PathStep(int index)
    {
        return Key is null
            ? $"{Type}[{index}]"
            : $"{Type}:{Convert.ToString(Key, CultureInfo.InvariantCulture)}";
    }

    internal static void Flatten(object? item, ICollection<Node> destination)
    {
        if (item is null)
        {
            return;
        }
        if (item is Node node)
        {
            destination.Add(node);
            return;
        }
        if (item is string value)
        {
            destination.Add(UI.Text(value));
            return;
        }
        if (item is IEnumerable enumerable)
        {
            foreach (object? nested in enumerable)
            {
                Flatten(nested, destination);
            }
            return;
        }
        throw new ArgumentException(
            $"A child must be a Node, string, or enumerable, not {item.GetType().FullName}.",
            nameof(item));
    }
}
