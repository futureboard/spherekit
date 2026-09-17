using SphereKit;
using System.Text.Json;

static void Expect(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
}

Node tree = UI.View(key: "root").Add(
    UI.Text("hello"),
    UI.Button("Ready", onPress: () => { }));
string json = tree.ToJson();
Expect(json.Contains("\"type\":\"#text\"", StringComparison.Ordinal), "text becomes a #text child");
Expect(json.Contains("\"id\":", StringComparison.Ordinal), "nodes receive identities");
Expect(UI.CheckTree(json.Insert(1, "\"revision\":1,")) == 4, "tree crosses the native validator");
Expect(UI.CheckCss(".page { display: flex; gap: 8px }") == 1, "stylesheet crosses the native parser");
Expect(UI.NodeTypes.Contains("menu-item"), "menu-item is advertised");

int pressed = 0;
double changed = 0;
Node sxaml = Sxaml.Load("""
    <View xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml" Class="page" x:Name="root">
      <Text x:Key="heading">hello from sxaml</Text>
      <Button Content="Ready" OnPress="Pressed" />
      <Slider Value="0.5" Minimum="0" Maximum="1" OnValueChange="Changed" />
    </View>
    """, new SxamlContext()
    .On("Pressed", () => pressed++)
    .On("Changed", value => changed = value));
string sxamlJson = sxaml.ToJson();
Expect(sxamlJson.Contains("\"className\":\"page\"", StringComparison.Ordinal), "SXAML maps Class");
Expect(sxamlJson.Contains("hello from sxaml", StringComparison.Ordinal), "SXAML maps element text");
Expect(sxamlJson.Contains("\"minimumValue\":0", StringComparison.Ordinal), "SXAML maps Minimum");
Expect(pressed == 0 && changed == 0, "SXAML callbacks are deferred until native events");
Expect(UI.CheckTree(sxamlJson.Insert(1, "\"revision\":2,")) == 5, "SXAML crosses the native validator");

try
{
    Sxaml.Load("<Button OnPress=\"Missing\" />");
    throw new InvalidOperationException("an unregistered SXAML handler was accepted");
}
catch (SxamlException)
{
    // Expected: declarative callbacks must be registered explicitly.
}

Node keyedA = UI.View().Add(UI.View(key: "a"), UI.View(key: "b"));
Node keyedB = UI.View().Add(UI.View(key: "b"), UI.View(key: "a"));
ulong IdAt(string value, int index) =>
    JsonDocument.Parse(value).RootElement.GetProperty("children")[0]
        .GetProperty("children")[index].GetProperty("id").GetUInt64();
Expect(IdAt(keyedA.ToJson(), 0) == IdAt(keyedB.ToJson(), 1), "keys preserve identity across reorder");
Expect(IdAt(keyedA.ToJson(), 1) == IdAt(keyedB.ToJson(), 0), "keys preserve the second identity");

try
{
    UI.CheckTree("{not json");
    throw new InvalidOperationException("invalid JSON was accepted");
}
catch (SphereKitValidationException)
{
    // Expected: validation is surfaced as a managed exception.
}

Console.WriteLine($"SphereKit {UI.Version}: smoke checks passed");
