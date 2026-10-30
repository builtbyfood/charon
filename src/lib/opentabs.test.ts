import { test } from "node:test";
import assert from "node:assert/strict";
import { restorePlan, toSaved } from "./opentabs.ts";
import type { Node } from "./api";

const nodes: Node[] = [
  { id: "f", parent_id: null, kind: "folder", name: "prod", sort: 0, settings: { color: "#f00", font: "Hack" } },
  { id: "web", parent_id: "f", kind: "ssh", name: "web-renamed", sort: 0, settings: { host: "10.0.0.1", font_size: 15 } },
  { id: "box", parent_id: null, kind: "serial", name: "console", sort: 0, settings: {} },
];

test("toSaved keeps terminals, drops the editor, elevated shells and moved-tab screens", () => {
  const w = toSaved([
    { id: "1", title: "web", color: "#f00", open: { kind: "ssh", nodeId: "web" } },
    { id: "editor", title: "editor" },
    { id: "2", title: "local", open: { kind: "local", admin: true } },
    { id: "3", title: "ops@h:2222", open: { kind: "ssh", user: "ops", host: "h", port: 2222 }, adopt: { big: "screen" } } as never,
  ], "3");
  assert.deepEqual(w, {
    tabs: [
      { title: "web", color: "#f00", font: undefined, open: { kind: "ssh", nodeId: "web" } },
      { title: "ops@h:2222", color: undefined, font: undefined, open: { kind: "ssh", user: "ops", host: "h", port: 2222 } },
    ],
    active: 1,
  });
  assert.equal(toSaved([{ id: "editor", title: "editor" }], "editor").active, 0);
});

test("restorePlan puts main first, refreshes saved connections and drops deleted ones", () => {
  const plan = restorePlan({
    "tab-z": { tabs: [{ title: "console", open: { kind: "serial", nodeId: "box" } }], active: 0 },
    main: {
      tabs: [
        { title: "gone", open: { kind: "ssh", nodeId: "deleted" } },
        { title: "local", open: { kind: "local" } },
        { title: "web", open: { kind: "ssh", nodeId: "web" } },
      ],
      active: 2,
    },
    "tab-a": { tabs: [{ title: "", open: { kind: "ssh", host: "h", user: "ops" } }, { title: "x", open: { kind: "ssh" } }], active: 0 },
    junk: "nope",
  }, nodes);
  assert.deepEqual(plan.tabs.map((t) => t.title), ["local", "web-renamed", "ssh", "console"]);
  // The main window's visible tab, after the deleted one fell out.
  assert.equal(plan.active, 1);
  // Colour and font come from the tree as it is now (folder colour + font, own size).
  assert.deepEqual(plan.tabs[1], { title: "web-renamed", color: "#f00", font: { family: "Hack", size: 15 }, open: { kind: "ssh", nodeId: "web" } });
});

test("restorePlan of nothing", () => {
  assert.deepEqual(restorePlan({}, nodes), { tabs: [], active: 0 });
});
