// Settings → Appearance: the theme mode choice, the one purely-local
// presentation preference.
import type { JSX } from "solid-js";
import { setThemeMode, themeMode } from "../../lib/theme";
import { Card, Field, SectionHeader, SegmentedControl } from "./rows";

export function AppearanceSection(): JSX.Element {
  return (
    <>
      <SectionHeader>Appearance</SectionHeader>
      <Card class="p-3.5">
        <Field title="Theme" subtitle="System follows your device's light or dark setting." />
        <div class="mt-3">
          <SegmentedControl
            ariaLabel="Theme"
            value={themeMode()}
            onChange={setThemeMode}
            options={[
              { value: "system", label: "System" },
              { value: "light", label: "Light" },
              { value: "dark", label: "Dark" },
            ]}
          />
        </div>
      </Card>
    </>
  );
}
