import { useMemo, useState } from "react";
import {
  Avatar,
  ScrollView,
  Text,
  TextField,
  View,
  type AvatarPresence,
} from "@spherekit/react";

interface Person {
  readonly name: string;
  readonly role: string;
  readonly presence: AvatarPresence;
}

/** Names in several scripts on purpose: they exercise shaping and font fallback. */
const PEOPLE: readonly Person[] = [
  { name: "Ada Lovelace", role: "Audio engine", presence: "online" },
  { name: "สมชาย ใจดี", role: "Localisation", presence: "online" },
  { name: "山田 花子", role: "Design", presence: "away" },
  { name: "Linus Pauling", role: "DSP", presence: "busy" },
  { name: "김민준", role: "Rendering", presence: "online" },
  { name: "Grace Hopper", role: "Build & release", presence: "offline" },
  { name: "Αλέξανδρος Παπαδόπουλος", role: "QA", presence: "away" },
  { name: "Margaret Hamilton", role: "Platform", presence: "online" },
];

const ORDER: Record<AvatarPresence, number> = { online: 0, away: 1, busy: 2, offline: 3 };

/**
 * A searchable roster.
 *
 * The filter is ordinary derived state. The names are the point: Thai,
 * Japanese, Korean and Greek arrive through the same shaping and fallback path
 * as Latin, and on Windows every glyph is rasterised by DirectWrite.
 */
export function Team() {
  const [query, setQuery] = useState("");
  const people = useMemo(() => {
    const q = query.trim().toLowerCase();
    return PEOPLE.filter(
      (p) => q === "" || p.name.toLowerCase().includes(q) || p.role.toLowerCase().includes(q),
    ).sort((a, b) => ORDER[a.presence] - ORDER[b.presence]);
  }, [query]);
  const online = PEOPLE.filter((p) => p.presence === "online").length;

  return (
    <View className="page">
      <View className="row">
        <TextField
          className="grow"
          value={query}
          placeholder="Search by name or role"
          onChange={setQuery}
        />
        <Text className="caption">{`${online} of ${PEOPLE.length} online`}</Text>
      </View>
      <ScrollView className="list">
        {people.length === 0 ? (
          <Text className="empty">{`No one matches “${query}”.`}</Text>
        ) : (
          people.map((person) => (
            <View key={person.name} className="list-row">
              <Avatar name={person.name} size={32} presence={person.presence} />
              <View className="grow person">
                <Text className="person-name">{person.name}</Text>
                <Text className="caption">{person.role}</Text>
              </View>
              <Text className={`presence ${person.presence}`}>{person.presence}</Text>
            </View>
          ))
        )}
      </ScrollView>
    </View>
  );
}
