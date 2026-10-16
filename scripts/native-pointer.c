/* Synthetic test pointer through the compositor's public virtual-pointer
 * protocol. Keeps pointer capability alive for the duration of desktop QA.
 * No device files, uinput, application hooks, or elevated privileges. */
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include "wlr-virtual-pointer-unstable-v1-client-protocol.h"

static struct zwlr_virtual_pointer_manager_v1 *manager;
static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data; (void)version;
    if (!strcmp(interface, "zwlr_virtual_pointer_manager_v1"))
        manager = wl_registry_bind(registry, name,
                                   &zwlr_virtual_pointer_manager_v1_interface, 1);
}
static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener listener = {.global=global, .global_remove=removed};
static uint32_t milliseconds(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t) != 0) return 0;
    return (uint32_t)((uint64_t)t.tv_sec * 1000 + t.tv_nsec / 1000000);
}
int main(void) {
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) return 2;
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &listener, NULL);
    if (wl_display_roundtrip(display) < 0 || !manager) return 2;
    struct zwlr_virtual_pointer_v1 *pointer =
        zwlr_virtual_pointer_manager_v1_create_virtual_pointer(manager, NULL);
    if (wl_display_roundtrip(display) < 0) return 2;
    /* Let other clients receive seat capability changes before pointer use. */
    usleep(250000);
    puts("ready"); fflush(stdout);
    char line[128], extra;
    unsigned x, y, width, height;
    int steps;
    while (fgets(line, sizeof(line), stdin)) {
        if (sscanf(line, "scroll %d %c", &steps, &extra) == 1) {
            if (steps < -20 || steps > 20 || !steps) return 2;
            zwlr_virtual_pointer_v1_axis_source(pointer, WL_POINTER_AXIS_SOURCE_WHEEL);
            zwlr_virtual_pointer_v1_axis_discrete(pointer, milliseconds(),
                WL_POINTER_AXIS_VERTICAL_SCROLL, wl_fixed_from_int(steps * 15), steps);
            zwlr_virtual_pointer_v1_frame(pointer);
            if (wl_display_roundtrip(display) < 0) return 2;
            puts("done"); fflush(stdout);
            continue;
        }
        uint32_t button = 0x110;
        int fields = sscanf(line, "click %u %u %u %u %c", &x, &y, &width, &height, &extra);
        if (fields != 4) {
            fields = sscanf(line, "middle %u %u %u %u %c", &x, &y, &width, &height, &extra);
            button = 0x112;
        }
        if (fields != 4
            || !width || !height || x >= width || y >= height) return 2;
        zwlr_virtual_pointer_v1_motion_absolute(pointer, milliseconds(), x, y, width, height);
        zwlr_virtual_pointer_v1_frame(pointer);
        if (wl_display_roundtrip(display) < 0) return 2;
        usleep(100000);
        zwlr_virtual_pointer_v1_button(pointer, milliseconds(), button, WL_POINTER_BUTTON_STATE_PRESSED);
        zwlr_virtual_pointer_v1_frame(pointer);
        if (wl_display_roundtrip(display) < 0) return 2;
        usleep(50000);
        zwlr_virtual_pointer_v1_button(pointer, milliseconds(), button, WL_POINTER_BUTTON_STATE_RELEASED);
        zwlr_virtual_pointer_v1_frame(pointer);
        if (wl_display_roundtrip(display) < 0) return 2;
        puts("done"); fflush(stdout);
    }
    zwlr_virtual_pointer_v1_destroy(pointer);
    zwlr_virtual_pointer_manager_v1_destroy(manager);
    wl_registry_destroy(registry);
    wl_display_disconnect(display);
    return 0;
}
