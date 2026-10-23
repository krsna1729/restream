// Which DaisyUI theme a viewer's choice resolves to. Pure, so it is tested
// without a DOM.

export type ThemeChoice = "system" | "dark" | "light";

/** The DaisyUI theme a choice resolves to, given the system preference. */
export function resolveTheme(choice: ThemeChoice, systemDark: boolean): "restream-dark" | "restream-light" {
  if (choice === "light") return "restream-light";
  if (choice === "dark") return "restream-dark";
  return systemDark ? "restream-dark" : "restream-light";
}
