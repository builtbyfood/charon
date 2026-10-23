import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";
import { installRafFallback } from "./lib/rafguard";

// Linux (WebKitGTK): frame callbacks can stall without input — see rafguard.ts.
// Logs how often the timer had to stand in for a frame, to confirm on a given box.
if (/Linux/.test(navigator.userAgent) && !/Android/.test(navigator.userAgent)) {
  const stats = installRafFallback(window);
  let seen = 0;
  console.info(`[charon] raf: fallback on, visibility=${document.visibilityState} focus=${document.hasFocus()}`);
  setInterval(() => {
    if (stats.fallback === seen) return;
    seen = stats.fallback;
    console.info(`[charon] raf: native=${stats.native} fallback=${stats.fallback} visibility=${document.visibilityState} focus=${document.hasFocus()}`);
  }, 5000);
}

mount(App, { target: document.getElementById("app")! });
