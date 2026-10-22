Adds WindowExtStartupNotify::activate_with_token for an existing window. Wayland uses xdg_activation_v1.activate with the desktop launcher token; X11 uses normal focus_window. Called via the eframe Frame wrapper on the GUI thread. No desktop-environment-specific commands.

Wayland now binds wl_data_device_manager and receives text/uri-list file drops on the window's connection. Copy only; 1 MiB/1000 URI bounds; nonblocking event-loop reads; local file URIs only; target coordinates are propagated as cursor events. Stalled reads expire. Native outgoing file drag sources are not yet implemented or claimed.

Wayland `focus_window` now requests xdg-activation using the initiating UUX surface and its recent pointer/keyboard event serial, then activates the target surface asynchronously. Requests expire after two seconds; no compositor-specific focus bypass is used. This enables user-triggered switching between existing UUX windows, including minimized targets.
