import { useMemo, useState } from "react";
import {
  Button,
  Checkbox,
  ScrollView,
  Text,
  TextField,
  View,
  cx,
} from "@spherekit/react";

interface Task {
  readonly id: number;
  readonly title: string;
  readonly done: boolean;
}

type Filter = "all" | "active" | "done";

const FILTERS: readonly { readonly key: Filter; readonly title: string }[] = [
  { key: "all", title: "All" },
  { key: "active", title: "Active" },
  { key: "done", title: "Done" },
];

const SEED: readonly Task[] = [
  { id: 1, title: "Bounce the stems for the mix review", done: false },
  { id: 2, title: "Replace wgpu with Direct3D 11", done: true },
  { id: 3, title: "Rasterise glyphs through DirectWrite", done: true },
  { id: 4, title: "Fall back to Skia when there is no GPU", done: true },
  { id: 5, title: "Send the session notes", done: false },
];

/**
 * A task list: the form-and-list half of any real application.
 *
 * Nothing here is SphereKit-specific beyond the host components. The text
 * field is controlled — every keystroke round-trips through React state and
 * back to the native field — and Enter commits through `onSubmit`.
 */
export function Tasks() {
  const [tasks, setTasks] = useState<readonly Task[]>(SEED);
  const [draft, setDraft] = useState("");
  const [filter, setFilter] = useState<Filter>("all");

  const add = (title: string) => {
    const trimmed = title.trim();
    if (trimmed === "") return;
    setTasks((all) => [...all, { id: Math.max(0, ...all.map((t) => t.id)) + 1, title: trimmed, done: false }]);
    setDraft("");
  };
  const toggle = (id: number, done: boolean) =>
    setTasks((all) => all.map((t) => (t.id === id ? { ...t, done } : t)));
  const remove = (id: number) => setTasks((all) => all.filter((t) => t.id !== id));

  const remaining = tasks.filter((t) => !t.done).length;
  const visible = useMemo(
    () =>
      tasks.filter((t) => (filter === "all" ? true : filter === "done" ? t.done : !t.done)),
    [tasks, filter],
  );

  return (
    <View className="page">
      <View className="row">
        <TextField
          className="grow"
          value={draft}
          placeholder="Add a task and press Enter"
          onChange={setDraft}
          onSubmit={add}
        />
        <Button className="primary" title="Add" disabled={draft.trim() === ""} onPress={() => add(draft)} />
      </View>

      <View className="row segmented">
        {FILTERS.map((f) => (
          <Button
            key={f.key}
            className={cx("segment", filter === f.key && "selected")}
            title={f.title}
            onPress={() => setFilter(f.key)}
          />
        ))}
        <View className="grow" />
        <Text className="caption">{`${remaining} remaining of ${tasks.length}`}</Text>
        <Button
          title="Clear done"
          disabled={tasks.every((t) => !t.done)}
          onPress={() => setTasks((all) => all.filter((t) => !t.done))}
        />
      </View>

      <ScrollView className="list">
        {visible.length === 0 ? (
          <Text className="empty">Nothing here.</Text>
        ) : (
          visible.map((task) => (
            <View key={task.id} className={cx("list-row", task.done && "done")}>
              <Checkbox checked={task.done} onChange={(done) => toggle(task.id, done)} />
              <Text className="grow task-title">{task.title}</Text>
              <Button className="ghost" title="Remove" onPress={() => remove(task.id)} />
            </View>
          ))
        )}
      </ScrollView>
    </View>
  );
}
