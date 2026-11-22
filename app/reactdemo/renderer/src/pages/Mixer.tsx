import { useEffect, useMemo, useState } from "react";
import {
  Button,
  Fader,
  Knob,
  Progress,
  Slider,
  Text,
  Toggle,
  View,
  cx,
  useNativeEvent,
  type SphereKitApiBridge,
} from "@spherekit/react";
import { Events, setAnimating, type MetersEvent } from "../native";

interface Channel {
  readonly name: string;
  readonly gain: number;
  readonly pan: number;
  readonly mute: boolean;
  readonly solo: boolean;
}

const INITIAL: readonly Channel[] = [
  { name: "Kick", gain: 0.82, pan: 0.5, mute: false, solo: false },
  { name: "Bass", gain: 0.7, pan: 0.45, mute: false, solo: false },
  { name: "Keys", gain: 0.55, pan: 0.3, mute: false, solo: false },
  { name: "Vocal", gain: 0.76, pan: 0.6, mute: false, solo: false },
];

/**
 * A vertical level meter.
 *
 * `Progress` only draws horizontally, so this is built from two views: a track
 * that anchors its child to the bottom, and a fill whose height is an inline
 * style. The fill turns amber and then red near the top, as a real meter does.
 */
function Meter({ level }: { readonly level: number }) {
  const percent = Math.round(Math.min(1, Math.max(0, level)) * 100);
  return (
    <View className="meter-track">
      <View
        className={cx("meter-fill", level > 0.9 ? "hot" : level > 0.7 && "warm")}
        style={{ height: `${percent}%` }}
      />
    </View>
  );
}

/** Linear gain to a decibel label. */
function decibels(gain: number): string {
  return gain <= 0.0001 ? "-inf" : (20 * Math.log10(gain)).toFixed(1);
}

/** Pan position as `L40`, `C` or `R25`. */
function panLabel(pan: number): string {
  const offset = Math.round((pan - 0.5) * 200);
  if (Math.abs(offset) < 2) return "C";
  return offset < 0 ? `L${-offset}` : `R${offset}`;
}

/**
 * A four-channel mixer.
 *
 * The meters are the interesting part: the levels are not computed here. The
 * Rust host synthesises them and raises a `meters` event every frame — but
 * only while this page has asked it to animate. That request is a method call
 * the other way (`app.setAnimating`), made from an effect, so leaving the tab
 * stops the redraw loop and the app goes back to drawing only on input.
 */
export function Mixer({ bridge }: { readonly bridge: SphereKitApiBridge }) {
  const [channels, setChannels] = useState<readonly Channel[]>(INITIAL);
  const [master, setMaster] = useState(0.8);
  const [levels, setLevels] = useState<readonly number[]>([0, 0, 0, 0]);
  const [running, setRunning] = useState(true);

  useEffect(() => {
    setAnimating(bridge, running);
    return () => setAnimating(bridge, false);
  }, [bridge, running]);

  useNativeEvent<MetersEvent>(bridge, Events.meters, (event) => setLevels(event.levels));

  const anySolo = channels.some((c) => c.solo);
  const update = (index: number, patch: Partial<Channel>) =>
    setChannels((all) => all.map((c, i) => (i === index ? { ...c, ...patch } : c)));

  // Post-fader, post-mute/solo levels: what you would actually hear.
  const post = channels.map((c, i) => {
    const audible = !c.mute && (!anySolo || c.solo);
    return audible ? Math.min(1, (levels[i] ?? 0) * c.gain * master * 1.25) : 0;
  });
  const masterLevel = useMemo(() => Math.min(1, Math.max(0, ...post)), [post]);

  return (
    <View className="page">
      <View className="row">
        <Text className="caption">
          {anySolo ? "Solo active — other channels are silent" : "Levels are raised by the host as a native event"}
        </Text>
        <View className="grow" />
        <Text className="caption">Signal</Text>
        <Toggle checked={running} onChange={setRunning} />
      </View>

      <View className="mixer">
        {channels.map((channel, index) => (
          <View key={channel.name} className={cx("strip", channel.mute && "muted")}>
            <Text className="strip-name">{channel.name}</Text>
            <Knob value={channel.pan} onValueChange={(pan) => update(index, { pan })} />
            <Text className="caption">{panLabel(channel.pan)}</Text>
            <View className="strip-body">
              <Fader value={channel.gain} onValueChange={(gain) => update(index, { gain })} />
              <Meter level={post[index] ?? 0} />
            </View>
            <Text className="value">{`${decibels(channel.gain)} dB`}</Text>
            <View className="row tight">
              <Button
                className={cx("ms", channel.mute && "mute-on")}
                title="M"
                onPress={() => update(index, { mute: !channel.mute })}
              />
              <Button
                className={cx("ms", channel.solo && "solo-on")}
                title="S"
                onPress={() => update(index, { solo: !channel.solo })}
              />
            </View>
          </View>
        ))}
      </View>

      <View className="card telemetry">
        <View className="row">
          <Text className="label">MASTER</Text>
          <View className="grow" />
          <View className="meter-readout">
            <Text className="value">{`${decibels(master)} dB`}</Text>
          </View>
        </View>
        <Slider value={master} step={0.001} onValueChange={setMaster} />
        <Progress value={masterLevel} />
      </View>
    </View>
  );
}
