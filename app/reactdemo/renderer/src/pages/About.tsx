import { version as reactVersion } from "react";
import { Button, Panel, Separator, Text, View, useInvoke, type SphereKitApiBridge } from "@spherekit/react";
import { Methods, quit, type AppInfo } from "../native";

function Row({ name, value }: { readonly name: string; readonly value: string }) {
  return (
    <View className="kv">
      <Text className="kv-name">{name}</Text>
      <Text className="kv-value">{value}</Text>
    </View>
  );
}

/**
 * What this app is running on, asked of the host through `app.info`.
 *
 * `useInvoke` is the request/response direction of the bridge: a promise that
 * the host resolves from Rust. The backend row is the one to watch — run with
 * `SPHEREKIT_BACKEND=skia` and the same React tree reports the CPU renderer.
 */
export function About({ bridge }: { readonly bridge: SphereKitApiBridge }) {
  const info = useInvoke<AppInfo>(bridge, Methods.info);

  return (
    <View className="page">
      <Panel className="card" title="Runtime">
        {info.loading ? (
          <Text className="caption">Asking the host…</Text>
        ) : info.error || !info.data ? (
          <Text className="caption">{`app.info failed: ${info.error?.message ?? "no data"}`}</Text>
        ) : (
          <View className="kv-list">
            <Row name="Renderer backend" value={info.data.backend} />
            <Row name="Adapter" value={info.data.adapter} />
            <Row name="Scale factor" value={`${info.data.scale.toFixed(2)}×`} />
            <Row name="Start-up" value={`${info.data.initMs.toFixed(0)} ms`} />
            <Separator />
            <Row name="JavaScript engine" value={`V8 ${info.data.v8}`} />
            <Row name="React" value={reactVersion} />
            <Row name="Bundle" value={info.data.renderer} />
          </View>
        )}
      </Panel>
      <Text className="caption">
        React reconciles in V8 and commits one tree per update across the API Bridge. Rust lowers
        it through the shared stylesheet into native SphereKit elements, which Direct3D 11 — or
        Skia on the CPU — draws. No browser, no WebView, no Node.
      </Text>
      <View className="row">
        <Button title="Refresh" onPress={info.refetch} />
        <View className="grow" />
        <Button className="danger" title="Quit" onPress={() => quit(bridge)} />
      </View>
    </View>
  );
}
