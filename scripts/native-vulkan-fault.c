/* Test-only Vulkan loader proxy. Poisons one disposable Frost device without
 * suspending the host or changing the user's desktop/GPU configuration. */
#include <vulkan/vulkan.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static PFN_vkGetInstanceProcAddr real_instance_proc;
static PFN_vkGetDeviceProcAddr real_device_proc;
static PFN_vkAcquireNextImageKHR real_acquire;

static void load_loader(void) {
    if (real_instance_proc) return;
    const char *path = getenv("FROST_TEST_REAL_VULKAN");
    void *loader = dlopen(path ? path : "/lib/x86_64-linux-gnu/libvulkan.so.1",
                          RTLD_NOW | RTLD_LOCAL);
    if (!loader) { fprintf(stderr, "test loader: %s\n", dlerror()); abort(); }
    real_instance_proc = (PFN_vkGetInstanceProcAddr)dlsym(loader, "vkGetInstanceProcAddr");
    real_device_proc = (PFN_vkGetDeviceProcAddr)dlsym(loader, "vkGetDeviceProcAddr");
    if (!real_instance_proc || !real_device_proc) abort();
}

static VKAPI_ATTR VkResult VKAPI_CALL acquire(VkDevice device, VkSwapchainKHR chain,
    uint64_t timeout, VkSemaphore semaphore, VkFence fence, uint32_t *index) {
    const char *trigger = getenv("FROST_TEST_VULKAN_FAULT");
    if (trigger && unlink(trigger) == 0) {
        fprintf(stderr, "test loader: injecting VK_ERROR_DEVICE_LOST\n");
        return VK_ERROR_DEVICE_LOST;
    }
    return real_acquire(device, chain, timeout, semaphore, fence, index);
}

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vkGetDeviceProcAddr(VkDevice device, const char *name) {
    load_loader();
    PFN_vkVoidFunction result = real_device_proc(device, name);
    if (result && strcmp(name, "vkAcquireNextImageKHR") == 0) {
        real_acquire = (PFN_vkAcquireNextImageKHR)result;
        return (PFN_vkVoidFunction)acquire;
    }
    return result;
}

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vkGetInstanceProcAddr(VkInstance instance, const char *name) {
    load_loader();
    if (strcmp(name, "vkGetDeviceProcAddr") == 0)
        return (PFN_vkVoidFunction)vkGetDeviceProcAddr;
    if (strcmp(name, "vkGetInstanceProcAddr") == 0)
        return (PFN_vkVoidFunction)vkGetInstanceProcAddr;
    return real_instance_proc(instance, name);
}
