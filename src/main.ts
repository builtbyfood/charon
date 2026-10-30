import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";
import { installRafFallback } from "./lib/rafguard";
import { forwardConsole } from "./lib/diag";
import { logFrontend } from "./lib/api";
import { applyRememberedTheme } from "./lib/themes";

// Paint in last run's theme straight away; App applies the profile's once loaded.
applyRememberedTheme();

// `[charon]` lines also reach stdout (release builds have no devtools).
forwardConsole(logFrontend);

// Linux (WebKitGTK): frame callbacks can stall without input — see rafguard.ts.
// Logs how often the timer had to stand in for a frame, to confirm on a given box.
if (/Linux/.test(navigator.userAgent) && !/Android/.test(navigator.userAgent)) {
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

mount(App, { target: document.getElementById("app")! });
