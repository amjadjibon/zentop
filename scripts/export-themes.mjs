// Resolves every palette through the same contrast-fitting rules the website
// uses and writes the semantic roles the native app embeds. Run after editing
// site/lib/definitions/themes.ts or site/lib/utils/theme-tokens.js.
import { readFileSync, writeFileSync } from "node:fs";
import vm from "node:vm";
import { themeTokens, themeIsDark } from "../site/lib/utils/theme-tokens.js";

export function loadThemes() {
  const source = readFileSync(
    new URL("../site/lib/definitions/themes.ts", import.meta.url),
    "utf8",
  );
  return vm.runInNewContext(
    "(" +
      source
        .slice(source.indexOf("= {") + 2)
        .trim()
        .replace(/;$/, "") +
      ")",
  );
}

export function exportThemes() {
  return Object.values(loadThemes()).map((theme) => {
    const tokens = themeTokens(theme.colors, { name: theme.name });
    return {
      name: theme.name,
      label: theme.label,
      dark: themeIsDark(theme.colors),
      radius: Number.parseFloat(tokens.radius) * 16,
      ...Object.fromEntries(
        Object.entries(tokens)
          .filter(([key]) => key !== "radius")
          .map(([key, value]) => [key.replaceAll("-", "_"), value]),
      ),
      yellow: theme.colors.yellow,
    };
  });
}

export const outputPath = new URL("../assets/themes.json", import.meta.url);

if (import.meta.url === `file://${process.argv[1]}`) {
  writeFileSync(outputPath, JSON.stringify(exportThemes(), null, 2) + "\n");
}
