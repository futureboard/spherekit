using System.Reflection;
using System.Runtime.InteropServices;

namespace SphereKit;

internal static class Native
{
    private const string LibraryName = "spherekit_dotnet";

    static Native()
    {
        NativeLibrary.SetDllImportResolver(typeof(Native).Assembly, ResolveLibrary);
    }

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal delegate nint RenderCallback(nint userData);

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal delegate byte EventCallback(nint userData, nint eventName, ulong nodeId, nint payloadJson);

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal delegate void FreeStringCallback(nint userData, nint value);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern nint spherekit_dotnet_version();

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern nint spherekit_dotnet_node_types();

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern nint spherekit_dotnet_last_error();

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern long spherekit_dotnet_check_tree(nint json);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern long spherekit_dotnet_check_css(nint css);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    internal static extern void spherekit_dotnet_quit();

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, ExactSpelling = true)]
    private static extern int spherekit_dotnet_run(
        nint title,
        float width,
        float height,
        nint css,
        byte dark,
        ulong frameLimit,
        byte hasFrameLimit,
        RenderCallback render,
        EventCallback onEvent,
        FreeStringCallback freeString,
        nint userData,
        out ulong frames);

    internal static string Version => ReadString(spherekit_dotnet_version());

    internal static string[] NodeTypes =>
        System.Text.Json.JsonSerializer.Deserialize<string[]>(ReadString(spherekit_dotnet_node_types()))
        ?? Array.Empty<string>();

    internal static long CheckTree(string json) => CheckText(spherekit_dotnet_check_tree, json, "tree");

    internal static long CheckCss(string css) => CheckText(spherekit_dotnet_check_css, css, "stylesheet");

    internal static ulong Run(
        string title,
        float width,
        float height,
        string css,
        bool dark,
        ulong? frameLimit,
        RenderCallback render,
        EventCallback onEvent,
        FreeStringCallback freeString)
    {
        nint titlePointer = Marshal.StringToCoTaskMemUTF8(title);
        nint cssPointer = Marshal.StringToCoTaskMemUTF8(css);
        try
        {
            int result = spherekit_dotnet_run(
                titlePointer,
                width,
                height,
                cssPointer,
                dark ? (byte)1 : (byte)0,
                frameLimit.GetValueOrDefault(),
                frameLimit.HasValue ? (byte)1 : (byte)0,
                render,
                onEvent,
                freeString,
                nint.Zero,
                out ulong frames);
            if (result != 0)
            {
                throw new SphereKitException(ReadError("native runner failed"));
            }
            return frames;
        }
        finally
        {
            Marshal.FreeCoTaskMem(titlePointer);
            Marshal.FreeCoTaskMem(cssPointer);
        }
    }

    private delegate long CheckFunction(nint value);

    private static long CheckText(CheckFunction check, string value, string kind)
    {
        nint pointer = Marshal.StringToCoTaskMemUTF8(value);
        try
        {
            long count = check(pointer);
            if (count < 0)
            {
                throw new SphereKitValidationException(ReadError($"invalid {kind}"));
            }
            return count;
        }
        finally
        {
            Marshal.FreeCoTaskMem(pointer);
        }
    }

    internal static string ReadError(string fallback)
    {
        nint pointer = spherekit_dotnet_last_error();
        return pointer == nint.Zero ? fallback : ReadString(pointer);
    }

    private static string ReadString(nint pointer)
    {
        return pointer == nint.Zero
            ? throw new SphereKitException("native library returned a null string")
            : Marshal.PtrToStringUTF8(pointer)
                ?? throw new SphereKitException("native library returned invalid UTF-8");
    }

    private static nint ResolveLibrary(string libraryName, Assembly assembly, DllImportSearchPath? searchPath)
    {
        if (!string.Equals(libraryName, LibraryName, StringComparison.Ordinal))
        {
            return nint.Zero;
        }

        foreach (string candidate in CandidatePaths())
        {
            if (!File.Exists(candidate))
            {
                continue;
            }
            try
            {
                return NativeLibrary.Load(candidate);
            }
            catch (DllNotFoundException)
            {
                // A matching filename with missing native dependencies should
                // still let the default loader produce its normal diagnostic.
            }
            catch (BadImageFormatException)
            {
                // Keep searching: debug/release or x86/x64 may be mixed.
            }
        }
        return nint.Zero;
    }

    private static IEnumerable<string> CandidatePaths()
    {
        string? overridePath = Environment.GetEnvironmentVariable("SPHEREKIT_LIB");
        if (!string.IsNullOrWhiteSpace(overridePath))
        {
            yield return Path.GetFullPath(overridePath);
        }

        string fileName = OperatingSystem.IsWindows()
            ? "spherekit_dotnet.dll"
            : OperatingSystem.IsMacOS()
                ? "libspherekit_dotnet.dylib"
                : "libspherekit_dotnet.so";

        for (DirectoryInfo? directory = new(AppContext.BaseDirectory);
             directory is not null;
             directory = directory.Parent)
        {
            yield return Path.Combine(directory.FullName, "target", "release", fileName);
            yield return Path.Combine(directory.FullName, "target", "debug", fileName);
        }
    }
}
