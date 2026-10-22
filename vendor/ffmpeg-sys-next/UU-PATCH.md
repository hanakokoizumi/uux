Based on crates.io ffmpeg-sys-next 9.0.1. The Vulkan bindgen shim's
`VkPhysicalDeviceFeatures2` size assertion now follows the target pointer ABI:
228 bytes on i686, 240 bytes on x86_64/aarch64. The C structure is unchanged;
its pointers and padding continue to use the compiler's native layout.

`ci/check-vulkan-abi.sh` compares size, alignment and field offsets with the
installed Khronos Vulkan headers on each build architecture. This does not
enable or claim Vulkan rendering/decoding in UUX.
