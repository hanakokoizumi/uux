#include <stddef.h>
#include <stdio.h>
#include <vulkan/vulkan.h>

int main(void) {
    printf("%zu %zu %zu %zu\n", sizeof(VkPhysicalDeviceFeatures2),
           _Alignof(VkPhysicalDeviceFeatures2),
           offsetof(VkPhysicalDeviceFeatures2, pNext),
           offsetof(VkPhysicalDeviceFeatures2, features));
    return 0;
}
