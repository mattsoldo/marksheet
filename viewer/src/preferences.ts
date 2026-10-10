export const THEMES = [
  { id: "paper", label: "Paper", description: "Quiet, warm, and typographic" },
  { id: "ledger", label: "Ledger", description: "Crisp gridlines with spreadsheet structure" },
  { id: "graphite", label: "Graphite", description: "Low-glare dark workspace" },
] as const;

export type ThemeId = (typeof THEMES)[number]["id"];

export interface ViewerPreferences {
  theme: ThemeId;
  sidebarOpen: boolean;
  detailsOpen: boolean;
}

const KEY = "marksheet.viewer.preferences";

export function isThemeId(value: unknown): value is ThemeId {
  return THEMES.some((theme) => theme.id === value);
}

export function defaultPreferences(prefersDark = false): ViewerPreferences {
  return { theme: prefersDark ? "graphite" : "paper", sidebarOpen: true, detailsOpen: false };
}

/** Browser storage is a convenience: unavailable or malformed values fall back to defaults. */
export function loadPreferences(storage: Storage | undefined, prefersDark = false): ViewerPreferences {
  const defaults = defaultPreferences(prefersDark);
  try {
    const raw = storage?.getItem(KEY);
    if (!raw) return defaults;
    const parsed = JSON.parse(raw) as Partial<Record<keyof ViewerPreferences, unknown>>;
    return {
      theme: isThemeId(parsed.theme) ? parsed.theme : defaults.theme,
      sidebarOpen: typeof parsed.sidebarOpen === "boolean" ? parsed.sidebarOpen : defaults.sidebarOpen,
      detailsOpen: typeof parsed.detailsOpen === "boolean" ? parsed.detailsOpen : defaults.detailsOpen,
    };
  } catch {
    return defaults;
  }
}

export function savePreferences(storage: Storage | undefined, preferences: ViewerPreferences): void {
  try {
    storage?.setItem(KEY, JSON.stringify(preferences));
  } catch {
    // Quota, privacy mode, or a disabled storage area: keep the in-memory choice.
  }
}

export function browserStorage(): Storage | undefined {
  try {
    return typeof localStorage === "undefined" ? undefined : localStorage;
  } catch {
    return undefined;
  }
}

export function prefersDarkScheme(): boolean {
  try {
    return typeof matchMedia === "function" && matchMedia("(prefers-color-scheme: dark)").matches;
  } catch {
    return false;
  }
}
