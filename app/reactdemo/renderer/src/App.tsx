import { useState } from "react";
import {
  Button,
  Text,
  View,
  cx,
  useInvoke,
  useNativeEvent,
  type SphereKitApiBridge,
} from "@spherekit/react";
import { Events, Methods, type AppInfo, type FrameEvent } from "./native";
import { About } from "./pages/About";
import { Mixer } from "./pages/Mixer";
import { Tasks } from "./pages/Tasks";
import { Team } from "./pages/Team";

/** Everything the app needs from the native side. */
export interface AppProps {
  /** The bridge this root was mounted on, for `invoke` and native events. */
  readonly bridge: SphereKitApiBridge;
}

const PAGES = [
  { key: "mixer", title: "Mixer" },
  { key: "tasks", title: "Tasks" },
  { key: "team", title: "Team" },
  { key: "about", title: "About" },
] as const;

type PageKey = (typeof PAGES)[number]["key"];

/**
 * SphereKit Studio: the demo application.
 *
 * Deliberately ordinary React — hooks, derived state, controlled inputs,
 * conditional rendering, lists with keys. Nothing here knows it is driving a
 * native renderer rather than a DOM; that is the point. The SphereKit-specific
 * parts are the host components and the bridge, used for the three things the
 * DOM has no equivalent for: native events (`frame`, `meters`), native method
 * calls (`app.info`, `app.setAnimating`, `app.quit`), and a stylesheet shared
 * with native code.
 */
export function App({ bridge }: AppProps) {
  const [page, setPage] = useState<PageKey>("mixer");
  const [frame, setFrame] = useState<FrameEvent>({ count: 0, fps: 0, cpuMs: 0 });
  const info = useInvoke<AppInfo>(bridge, Methods.info);

  useNativeEvent<FrameEvent>(bridge, Events.frame, setFrame);
  // Native code can drive navigation too — a menu, a shortcut, or the
  // headless `--screenshot --page` mode in `src/main.rs`.
  useNativeEvent<{ page: string }>(bridge, Events.navigate, ({ page: next }) => {
    if (PAGES.some((p) => p.key === next)) setPage(next as PageKey);
  });

  return (
    <View className="root">
      <View className="sidebar">
        <View className="brand">
          <Text className="brand-title">SphereKit</Text>
          <Text className="brand-subtitle">Studio</Text>
        </View>
        {PAGES.map((p) => (
          <Button
            key={p.key}
            className={cx("nav", page === p.key && "selected")}
            title={p.title}
            onPress={() => setPage(p.key)}
          />
        ))}
        <View className="grow" />
        <View className="badge">
          <Text className="badge-text">{info.data ? info.data.backend : "…"}</Text>
        </View>
      </View>

      <View className="main">
        <View className="header">
          <View className="heading">
            <Text className="title">{PAGES.find((p) => p.key === page)?.title ?? ""}</Text>
            <Text className="subtitle">React 19 in V8 · rendered natively</Text>
          </View>
          <View className="grow" />
          <Text className="stat">{`${frame.fps.toFixed(0)} fps`}</Text>
          <Text className="stat">{`${frame.cpuMs.toFixed(2)} ms cpu`}</Text>
          <Text className="stat">{`frame ${frame.count}`}</Text>
        </View>

        {page === "mixer" && <Mixer bridge={bridge} />}
        {page === "tasks" && <Tasks />}
        {page === "team" && <Team />}
        {page === "about" && <About bridge={bridge} />}
      </View>
    </View>
  );
}
