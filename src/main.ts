import { forwardConsole } from "./lib/diag";
import { logFrontend, repaintPoke } from "./lib/api";
import { installRafFallback } from "./lib/rafguard";

// CHARON_BISECT (see src-tauri/src/bisect.rs): set by an init script before this runs.
const bisect: string[] = (window as unknown as { __CHARON_BISECT__?: string[] }).__CHARON_BISECT__ ?? [];
const page = bisect.find((f) => f.startsWith("page="))?.slice(5);

if (page === "bare" || page === "bare-ipc" || page === "css") {
  barePage(page);
} else {
  void startApp();
}

// Only a counter (a bare create-tauri-app's page, near enough): `bare` has no CSS
// and no IPC, `bare-ipc` adds the IPC the app's debug ticker makes each tick (a
// repaint poke + a log line), `css` adds the app's stylesheets and theme too.
function barePage(level: string): void {
  if (level === "css") {
    void import("./app.css");
    void import("./lib/themes").then((t) => t.applyRememberedTheme());
  } else {
    // xterm.css comes from index.html.
    document.querySelectorAll('link[rel="stylesheet"], style').forEach((e) => e.remove());
  }
  const app = document.getElementById("app")!;
  app.textContent = `bisect ${bisect.join(",")}: tick 0`;
  let n = 0;
  setInterval(() => {
    n++;
    app.textContent = `bisect ${bisect.join(",")}: tick ${n}`;
    if (level !== "bare") {
      void repaintPoke()
        .catch(() => "?")
        .then((mode) => logFrontend("info", `[charon] ticker n=${n} t=${(performance.timeOrigin + performance.now()).toFixed(1)} repaint=${mode} page=${level}`));
    }
  }, 250);
}

async function startApp(): Promise<void> {
  await import("./app.css");
  const { applyRememberedTheme } = await import("./lib/themes");
  // Paint in last run's theme straight away; App applies the profile's once loaded.
  applyRememberedTheme();

  // `[charon]` lines also reach stdout (release builds have no devtools).
  if (!bisect.includes("noconsole")) forwardConsole(logFrontend);
  if (bisect.length) console.info(`[charon] bisect page: ${bisect.join(",")}`);

  // Linux (WebKitGTK): frame callbacks can stall without input — see rafguard.ts.
  // Logs how often the timer had to stand in for a frame, to confirm on a given box.
  if (/Linux/.test(navigator.userAgent) && !/Android/.test(navigator.userAgent) && !bisect.includes("noraf")) {
    const stats = installRafFallback(window);
    let seen = 0;
    console.info(`[charon] raf: fallback on, visibility=${document.visibilityState} focus=${document.hasFocus()}`);
    let reported = false;
    setInterval(() => {
      if (stats.fallback === seen) {
        // Say once that frames are arriving on their own, so "never fired" is visible too.
        if (!reported && stats.fallback === 0 && stats.native > 0) { reported = true; console.info(`[charon] raf: native=${stats.native} fallback=0 (timer not needed so far)`); }
        return;
      }
      seen = stats.fallback;
      console.info(`[charon] raf: native=${stats.native} fallback=${stats.fallback} visibility=${document.visibilityState} focus=${document.hasFocus()}`);
    }, 5000);
  }

  const { mount } = await import("svelte");
  const { default: App } = await import("./App.svelte");
  mount(App, { target: document.getElementById("app")! });
}
