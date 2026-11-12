// Which terminal renderer to load (Settings → Terminal). Pure, node-tested.

export type RendererSetting = "auto" | "webgl" | "canvas" | "dom";
export function rendererSetting(s: string | undefined): RendererSetting {
  return s === "webgl" || s === "canvas" || s === "dom" ? s : "auto";
}
/** Linux's WebKitGTK webview, where WebGL has been the renderer that fails to paint. */
export function isLinuxWebview(ua = typeof navigator === "undefined" ? "" : navigator.userAgent): boolean {
  return /Linux/.test(ua) && !/Android/.test(ua);
}
/**
 * "auto": the DOM renderer on Linux (it paints reliably under WebKitGTK; WebGL
 * there is opt-in), WebGL elsewhere. A renderer named in the profile is used as is.
 */
export function resolveRenderer(s: RendererSetting, linux = isLinuxWebview()): "webgl" | "canvas" | "dom" {
  if (s !== "auto") return s;
  return linux ? "dom" : "webgl";
}
