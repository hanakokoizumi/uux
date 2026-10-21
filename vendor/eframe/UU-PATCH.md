# UU Desktop immediate viewport presentation

Based on the locally pinned upstream `eframe 0.37.0` crate. Changes are limited
to `src/native/glow_integration.rs`, its small scheduling policy helper
`src/native/immediate_visibility.rs`, and the helper's declaration in
`src/native/mod.rs`.

The root/deferred path respects `ViewportInfo::visible()` and notifies winit
before presenting. The immediate path used by the remote and file windows did
neither. It could render and swap a minimized/occluded surface on the same
thread that handles buttons in every window.

This patch keeps immediate UI callbacks and shared texture updates running,
but skips drawing/presentation for known hidden or zero-sized surfaces. Visible
immediate surfaces call `pre_present_notify()` before swapping, matching the
root's Wayland frame-callback integration. Lack of keyboard focus alone never
suppresses rendering. Unknown visibility keeps the upstream behavior.

The application also stops issuing `RequestPaintWhileHidden` for its root:
the parent UI now runs while a descendant is visible **including LogicOnly
passes**. Upstream only allowed this on Full passes. On Wayland the compositor
withholds a hidden parent's frame callbacks, leaving only LogicOnly passes;
without this fix a visible, focused immediate child freezes too. The parent
surface itself is never painted/swapped in LogicOnly mode.

The initial presentation-only patch failed the moving-video regression when
the root was minimized: child draw calls dropped to zero for 30 seconds while
video decoding continued. This failure is retained in
`../../../captures/uux/perf/focus-motion-virtual-after-01`. The visibility test covers the
missing parent frame callback with a visible child.

The repeat moving-video regression, `focus-motion-virtual-after-02`, passed
the original nonzero-drawing guard, but analysis of the minimized-root interval
found just 3.94 canvas fps against 21 decoded fps. Its original result is retained;
it is not evidence of smooth rendering. The parent still had to wait for the
250 ms missed-redraw fallback after a child's redraw was redirected to it.
The final routing patch runs the parent's UI directly on child redraw, using
LogicOnly to avoid presenting the parent's surface. The strengthened regression
requires the visible child's median frame rate to keep up with at least 75% of
the decoded frame rate during the minimized-root interval.

`focus-motion-virtual-after-03` passes the stronger check: 20.07 canvas fps
versus 20 decoded fps over that interval, with no zero-drawing sample. All 369
GUI response probes returned, with a 55.65 ms maximum. Retained artifacts and
the report generator are described in [performance notes](../../../docs/Performance-Optimization.md).

A separate two-window lifecycle run confirmed that closing one window keeps
the other connected and closing the last window releases the session. These
runs use an isolated virtual Wayland compositor, not the user's physical desktop.

This addresses a concrete presentation-path inconsistency. The user's
multi-second focus-return stall has not yet been reproduced under the measured
static-desktop workload; do not claim this patch proves that stall's cause.

## Existing-window activation

Frame::activate_with_token forwards a desktop launch token to the vendored winit WindowExtStartupNotify implementation. It is used on the GUI thread after receiving a single-instance activation request. See vendor/winit/UU-PATCH.md.
