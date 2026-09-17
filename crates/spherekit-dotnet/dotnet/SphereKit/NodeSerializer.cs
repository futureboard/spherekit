using System.Collections;
using System.Text.Json;
using System.Text.Encodings.Web;

namespace SphereKit;

internal static class NodeSerializer
{
    private static readonly JsonSerializerOptions Options = new()
    {
        Encoder = JavaScriptEncoder.Default,
        WriteIndented = false,
    };

    internal static string Serialize(
        IEnumerable<Node> roots,
        out Dictionary<ulong, IReadOnlyDictionary<string, Action<object?>>> handlers)
    {
        var handlerMap = new Dictionary<ulong, IReadOnlyDictionary<string, Action<object?>>>();
        var seen = new HashSet<ulong>();
        var children = new List<object?>();
        int index = 0;
        foreach (Node node in roots)
        {
            children.Add(Walk(node, node.PathStep(index++), seen, handlerMap));
        }

        var tree = new SortedDictionary<string, object?>(StringComparer.Ordinal)
        {
            ["children"] = children,
        };
        handlers = handlerMap;
        return JsonSerializer.Serialize(tree, Options);
    }

    private static SortedDictionary<string, object?> Walk(
        Node node,
        string path,
        ISet<ulong> seen,
        IDictionary<ulong, IReadOnlyDictionary<string, Action<object?>>> handlers)
    {
        ulong identity = Fnv1a(path);
        while (!seen.Add(identity))
        {
            identity = Fnv1a($"{path}\0{identity}");
        }

        if (node.Handlers.Count > 0)
        {
            handlers[identity] = new Dictionary<string, Action<object?>>(node.Handlers);
        }

        var output = new SortedDictionary<string, object?>(StringComparer.Ordinal)
        {
            ["id"] = identity,
            ["type"] = node.Type,
        };
        if (node.Props.Count > 0)
        {
            output["props"] = SortObject(node.Props);
        }
        if (node.Text is not null)
        {
            output["text"] = node.Text;
        }
        if (node.Children.Count > 0)
        {
            var childValues = new List<object?>();
            for (int index = 0; index < node.Children.Count; index++)
            {
                Node child = node.Children[index];
                childValues.Add(Walk(child, $"{path}/{child.PathStep(index)}", seen, handlers));
            }
            output["children"] = childValues;
        }
        return output;
    }

    internal static ulong Fnv1a(string text)
    {
        const ulong offset = 0xCBF29CE484222325;
        const ulong prime = 0x100000001B3;
        ulong digest = offset;
        foreach (byte value in System.Text.Encoding.UTF8.GetBytes(text))
        {
            digest = (digest ^ value) * prime;
        }
        return digest == 0 ? 1 : digest;
    }

    private static object SortObject(object value)
    {
        if (value is IDictionary<string, object?> dictionary)
        {
            var sorted = new SortedDictionary<string, object?>(StringComparer.Ordinal);
            foreach (KeyValuePair<string, object?> entry in dictionary)
            {
                sorted[entry.Key] = entry.Value is null ? null : SortObject(entry.Value);
            }
            return sorted;
        }
        if (value is IDictionary nonGeneric)
        {
            var sorted = new SortedDictionary<string, object?>(StringComparer.Ordinal);
            foreach (DictionaryEntry entry in nonGeneric)
            {
                if (entry.Key is string key)
                {
                    sorted[key] = entry.Value is null ? null : SortObject(entry.Value);
                }
            }
            return sorted;
        }
        if (value is IEnumerable enumerable && value is not string)
        {
            return enumerable.Cast<object?>().Select(item => item is null ? null : SortObject(item)).ToList();
        }
        return value;
    }
}
