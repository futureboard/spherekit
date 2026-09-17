using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;
using System.Text.Json;

namespace SphereKit;

/// <summary>A retained-tree SphereKit application driven by C#.</summary>
/// <remarks>
/// <para>
/// Subclass this type, keep application state on the subclass, and return a
/// fresh node tree from <see cref="Render"/>. The native side commits only
/// when the serialized tree changes, so there is no invalidation method to
/// call after a state change.
/// </para>
/// <para>
/// The event loop runs on the caller's thread where the platform permits it.
/// On Windows, an MTA console caller is moved to a dedicated STA UI thread so
/// winit/OLE can initialize correctly.
/// </para>
/// </remarks>
public abstract class App
{
    private bool _setupComplete;

    /// <summary>Window title.</summary>
    public string Title { get; set; } = "SphereKit";

    /// <summary>Window width in logical pixels.</summary>
    public float Width { get; set; } = 900;

    /// <summary>Window height in logical pixels.</summary>
    public float Height { get; set; } = 620;

    /// <summary>Stylesheet installed before the first frame.</summary>
    public string Css { get; set; } = string.Empty;

    /// <summary>Whether the native theme is dark.</summary>
    public bool Dark { get; set; } = true;

    /// <summary>Number of render callbacks completed by this app.</summary>
    public ulong Frames { get; private set; }

    /// <summary>Runs once before the first window run.</summary>
    protected virtual void Setup() { }

    /// <summary>Builds the current frame's tree.</summary>
    public abstract Node Render();

    /// <summary>Runs the window until it closes or the optional frame limit.</summary>
    public void Run(ulong? frameLimit = null)
    {
        if (!_setupComplete)
        {
            Setup();
            _setupComplete = true;
        }
        var state = new CallbackState();
        Native.RenderCallback render = _ =>
        {
            try
            {
                Node root = Render() ?? throw new InvalidOperationException("Render() returned null.");
                string json = NodeSerializer.Serialize(
                    new[] { root },
                    out Dictionary<ulong, IReadOnlyDictionary<string, Action<object?>>> handlers);
                state.Handlers = handlers;
                Frames++;
                return Marshal.StringToCoTaskMemUTF8(json);
            }
            catch (Exception exception)
            {
                state.Exception = exception;
                return nint.Zero;
            }
        };
        Native.EventCallback onEvent = (_, eventName, nodeId, payloadJson) =>
        {
            try
            {
                string name = Marshal.PtrToStringUTF8(eventName)
                    ?? throw new SphereKitException("native event name was not valid UTF-8");
                JsonElement? payload;
                if (payloadJson == nint.Zero)
                {
                    payload = null;
                }
                else
                {
                    using JsonDocument document = JsonDocument.Parse(Marshal.PtrToStringUTF8(payloadJson)
                        ?? throw new SphereKitException("native event payload was not valid UTF-8"));
                    payload = document.RootElement.Clone();
                }
                Dispatch(state.Handlers, name, nodeId, payload);
                return (byte)1;
            }
            catch (Exception exception)
            {
                state.Exception = exception;
                return (byte)0;
            }
        };
        Native.FreeStringCallback freeString = (_, value) =>
        {
            if (value != nint.Zero)
            {
                Marshal.FreeCoTaskMem(value);
            }
        };

        Exception? nativeException = null;
        void Execute()
        {
            try
            {
                Native.Run(Title, Width, Height, Css, Dark, frameLimit, render, onEvent, freeString);
            }
            catch (Exception exception)
            {
                nativeException = exception;
            }
        }

        // winit needs an STA/OLE apartment on Windows. A normal console .NET
        // entry point is MTA, so use a dedicated STA UI thread unless the
        // caller already opted into STA (WinForms/WPF apps normally do).
        if (OperatingSystem.IsWindows()
            && Thread.CurrentThread.GetApartmentState() != ApartmentState.STA)
        {
            var uiThread = new Thread(Execute)
            {
                IsBackground = false,
                Name = "SphereKit UI",
            };
#pragma warning disable SYSLIB0006
            uiThread.SetApartmentState(ApartmentState.STA);
#pragma warning restore SYSLIB0006
            uiThread.Start();
            uiThread.Join();
        }
        else
        {
            Execute();
        }

        if (state.Exception is not null)
        {
            ExceptionDispatchInfo.Capture(state.Exception).Throw();
        }
        if (nativeException is not null)
        {
            throw nativeException;
        }
    }

    /// <summary>Requests the active window to close on its next event turn.</summary>
    public static void Quit() => UI.Quit();

    private static void Dispatch(
        IReadOnlyDictionary<ulong, IReadOnlyDictionary<string, Action<object?>>> handlers,
        string eventName,
        ulong nodeId,
        JsonElement? payload)
    {
        if (!handlers.TryGetValue(nodeId, out IReadOnlyDictionary<string, Action<object?>>? nodeHandlers)
            || !nodeHandlers.TryGetValue(eventName, out Action<object?>? handler))
        {
            return;
        }

        if (payload is null)
        {
            handler(null);
            return;
        }

        object? value = JsonValue(payload.Value);
        if (value is Dictionary<string, object?> values && values.Count == 1)
        {
            handler(values.Values.First());
        }
        else
        {
            handler(value);
        }
    }

    private static object? JsonValue(JsonElement value)
    {
        return value.ValueKind switch
        {
            JsonValueKind.Null or JsonValueKind.Undefined => null,
            JsonValueKind.True => true,
            JsonValueKind.False => false,
            JsonValueKind.String => value.GetString(),
            JsonValueKind.Number => value.TryGetInt64(out long integer) ? integer : value.GetDouble(),
            JsonValueKind.Array => value.EnumerateArray().Select(JsonValue).ToList(),
            JsonValueKind.Object => value.EnumerateObject()
                .ToDictionary(property => property.Name, property => JsonValue(property.Value), StringComparer.Ordinal),
            _ => null,
        };
    }

    private sealed class CallbackState
    {
        internal Exception? Exception { get; set; }
        internal IReadOnlyDictionary<ulong, IReadOnlyDictionary<string, Action<object?>>> Handlers { get; set; } =
            new Dictionary<ulong, IReadOnlyDictionary<string, Action<object?>>>();
    }
}
