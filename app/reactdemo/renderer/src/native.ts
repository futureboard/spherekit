/**
 * The contract between this renderer and the Rust host in `src/main.rs`.
 *
 * Every method and event the app uses is named here once, with its payload
 * type, so a rename on either side is one grep away and the components never
 * spell a method string themselves.
 */
import type { SphereKitApiBridge } from "@spherekit/react";

/** What `app.info` returns. Filled in once the native surface exists. */
export interface AppInfo {
  /** `"D3D11"` or `"Skia"`, after `Auto` and `SPHEREKIT_BACKEND` resolved. */
  readonly backend: string;
  /** The adapter, e.g. `Direct3D 11 (NVIDIA GeForce …, FL 11_1)`. */
  readonly adapter: string;
  /** V8's own version string. */
  readonly v8: string;
  /** `"react"` when Bun bundled this file; the host reports what it loaded. */
  readonly renderer: string;
  /** Device pixels per logical pixel. */
  readonly scale: number;
  /** Milliseconds the renderer and the font scan took at start-up. */
  readonly initMs: number;
}

/** Raised every presented frame. */
export interface FrameEvent {
  readonly count: number;
  /** Frames per second over the last half second. */
  readonly fps: number;
  /** CPU time the last frame spent building, laying out and painting. */
  readonly cpuMs: number;
}

/** Raised every frame while the mixer is animating: one level per channel, 0..1. */
export interface MetersEvent {
  readonly levels: readonly number[];
}

export const Methods = {
  info: "app.info",
  quit: "app.quit",
  setAnimating: "app.setAnimating",
} as const;

export const Events = {
  frame: "frame",
  meters: "meters",
  /** `{ page }`: switch to a page by key. */
  navigate: "navigate",
} as const;

/** Asks the host to redraw continuously (meters) or only on input. */
export function setAnimating(bridge: SphereKitApiBridge, on: boolean): void {
  void bridge.invoke(Methods.setAnimating, { on });
}

/** Ends the application. */
export function quit(bridge: SphereKitApiBridge): void {
  void bridge.invoke(Methods.quit);
}
