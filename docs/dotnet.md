# .NET

`crates/spherekit-dotnet` is the C#/.NET wrapper for SphereKit. It follows the
same boundary as the Python wrapper:

```text
C# Render() -> deterministic JSON tree -> Rust ReactHost -> layout -> GPU
C# handlers <- native event callback  <- Rust EventQueue
```

C# owns application state, node builders, serialization, and managed event
handlers. Rust owns the desktop event loop, GPU surface, CSS cascade, layout,
and widget lowering. The native part is a small `cdylib` with a C ABI, so the
managed package has no C++/CLI dependency.

## Build and run

From the repository root:

```bash
cargo build -p spherekit-dotnet --release
dotnet run --project crates/spherekit-dotnet/dotnet/SphereKit.Demo/SphereKit.Demo.csproj -- --frames 120
dotnet run --project crates/spherekit-dotnet/dotnet/SphereKit.SxamlDemo/SphereKit.SxamlDemo.csproj -- --frames 120
```

The C# loader searches `target/release` and `target/debug` above the managed
assembly directory. `SPHEREKIT_LIB` can point to a specific native library:

```powershell
$env:SPHEREKIT_LIB = "W:\path\to\spherekit_dotnet.dll"
dotnet run --project crates/spherekit-dotnet/dotnet/SphereKit.Demo/SphereKit.Demo.csproj
```

The `SphereKit` class library targets `net8.0` and uses only the BCL. It is
currently checkout-based rather than a NuGet package because the native binary
must be built for the target platform alongside the application.

## Minimal application

```csharp
using SphereKit;

sealed class Counter : App
{
    private int count;

    public Counter()
    {
        Title = "Counter";
        Css = ".page { display: flex; flex-direction: column; gap: 12px; padding: 24px }";
    }

    public override Node Render() => UI.View("page").Add(
        UI.Text($"Count: {count}"),
        UI.Button("Add one", onPress: () => count++),
        UI.Button("Quit", onPress: App.Quit));
}

new Counter().Run();
```

`Node.Add` accepts nodes, strings, `null`, and nested `IEnumerable` values, so
conditional and LINQ-built children do not need a special fragment type.
`key:` is used in the identity path for list rows, matching the Python/React
identity rule. `Node.ToJson()` is available for diagnostics; `App.Run()` owns
the monotonically increasing revision that the native host validates.

The built-in builders are `View`, `Text`, `Button`, `Slider`, `Knob`, `Fader`,
`Toggle`, `Checkbox`, `TextField`, `Progress`, `Separator`, `Panel`,
`ScrollView`, `Avatar`, and `MenuItem`. Event callbacks are typed as
`Action`, `Action<double>`, `Action<bool>`, or `Action<string>` according to
the widget.

## SXAML

The wrapper also supports SXAML, a small XAML-shaped syntax that lowers to the
same `Node` tree. It supports built-in elements, `Class`, `x:Name`, `x:Key`,
inline `Style`, scalar props, and `OnPress`, `OnValueChange`, `OnChange`,
`OnSubmit`, and `OnSelect` callbacks:

```csharp
private const string Markup = """
    <View xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml" Class="page" x:Name="root">
      <Text Class="title">SXAML control surface</Text>
      <Button Content="Add one" OnPress="Add" />
      <Slider Value="0.5" Minimum="0" Maximum="1" OnValueChange="SetVolume" />
    </View>
    """;

public override Node Render() => Sxaml.Load(Markup,
    new SxamlContext()
        .On("Add", Add)
        .On("SetVolume", SetVolume));
```

`Sxaml.LoadFile` reads a `.sxaml` file, while `Sxaml.LoadMany` accepts a
`<Sxaml>` or `<Fragment>` wrapper for sibling roots. `StackPanel`,
`HorizontalStack`, `VerticalStack`, `Grid`, `Label`, `TextBlock`, and
`Divider` are convenience aliases; unknown widget elements are rejected with
`SxamlException`.

The runnable `SphereKit.SxamlDemo` keeps its layout in
`crates/spherekit-dotnet/dotnet/SphereKit.SxamlDemo/Counter.sxaml` and its
event handlers in `Counter.sxaml.cs` as code-behind. `Program.cs` only starts
the app. The project embeds the `.sxaml` file into the assembly, so build
output contains no standalone SXAML file; the code-behind reads the embedded
template before calling `Sxaml.Load`.

`UI.CheckTree` and `UI.CheckCss` validate input without opening a window. They
are intended for tests because a rejected commit otherwise leaves the previous
frame on screen.

On Windows, `App.Run()` uses a dedicated STA thread when the caller is running
from the usual MTA console thread. This satisfies winit/OLE initialization
without requiring every console application's entry point to add `[STAThread]`.
