// Theme selection (docs/web-client.md). The tokens live in theme.css; this
// module only decides which set is active: an explicit choice persisted in
// localStorage, or the system scheme when none is stored. The resolved theme
// lands as data-theme on <html>, which is the selector theme.css keys on.

export type ThemeName = "after-dark" | "daylight";
export type ThemeChoice = ThemeName | "system";

const KEY = "theme";

const lightQuery = () => window.matchMedia("(prefers-color-scheme: light)");

const systemTheme = (): ThemeName =>
  lightQuery().matches ? "daylight" : "after-dark";

export function themeChoice(): ThemeChoice {
  const stored = localStorage.getItem(KEY);
  return stored === "after-dark" || stored === "daylight" ? stored : "system";
}

/** The standalone window's chrome takes `theme-color`, so an installed app
 * whose title bar stays dark under Daylight looks broken in a way the page
 * itself never does. index.html carries After Dark's value as the pre-JS
 * default; this keeps it following the resolved theme. */
function applyThemeColor() {
  const meta = document.querySelector('meta[name="theme-color"]');
  if (!(meta instanceof HTMLMetaElement)) return;
  const page = getComputedStyle(document.documentElement)
    .getPropertyValue("--bg-page")
    .trim();
  if (page !== "") meta.content = page;
}

function apply() {
  const choice = themeChoice();
  document.documentElement.dataset.theme =
    choice === "system" ? systemTheme() : choice;
  applyThemeColor();
}

export function setThemeChoice(choice: ThemeChoice) {
  if (choice === "system") {
    localStorage.removeItem(KEY);
  } else {
    localStorage.setItem(KEY, choice);
  }
  apply();
}

/** Called once before first render. The scheme listener stays registered
 * either way — `apply` re-reads the stored choice, so a flip only shows
 * through when the choice is "system". */
export function initTheme() {
  apply();
  lightQuery().addEventListener("change", apply);
}
