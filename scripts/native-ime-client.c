/* Minimal real Wayland input-method-v2 fixture. Not an application mock.
 * Only emits fixed synthetic preedit/commit through the compositor protocol. */
#include <errno.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include "input-method-unstable-v2-client-protocol.h"

struct state {
    struct wl_display *display;
    struct wl_seat *seat;
    struct zwp_input_method_manager_v2 *manager;
    struct zwp_input_method_v2 *method;
    uint32_t serial;
    bool pending_active, active, unavailable;
};
static void activate(void *data, struct zwp_input_method_v2 *method) {
    (void)method; ((struct state *)data)->pending_active = true;
}
static void deactivate(void *data, struct zwp_input_method_v2 *method) {
    (void)method; ((struct state *)data)->pending_active = false;
}
static void surrounding(void *data, struct zwp_input_method_v2 *method,
                        const char *text, uint32_t cursor, uint32_t anchor) {
    (void)data; (void)method; (void)text; (void)cursor; (void)anchor;
}
static void cause(void *data, struct zwp_input_method_v2 *method, uint32_t value) {
    (void)data; (void)method; (void)value;
}
static void content(void *data, struct zwp_input_method_v2 *method,
                    uint32_t hint, uint32_t purpose) {
    (void)data; (void)method; (void)hint; (void)purpose;
}
static void done(void *data, struct zwp_input_method_v2 *method) {
    (void)method; struct state *s = data;
    s->serial++; s->active = s->pending_active;
    printf("done serial=%u active=%d\n", s->serial, s->active); fflush(stdout);
}
static void unavailable(void *data, struct zwp_input_method_v2 *method) {
    (void)method; ((struct state *)data)->unavailable = true;
}
static const struct zwp_input_method_v2_listener method_listener = {
    .activate=activate, .deactivate=deactivate, .surrounding_text=surrounding,
    .text_change_cause=cause, .content_type=content, .done=done, .unavailable=unavailable
};
static void seat_caps(void *data, struct wl_seat *seat, uint32_t capabilities) {
    (void)data; (void)seat; (void)capabilities;
}
static void seat_name(void *data, struct wl_seat *seat, const char *name) {
    (void)data; (void)seat; (void)name;
}
static const struct wl_seat_listener seat_listener = {.capabilities=seat_caps, .name=seat_name};
static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)version; struct state *s = data;
    if (!strcmp(interface, "wl_seat") && !s->seat) {
        s->seat = wl_registry_bind(registry, name, &wl_seat_interface, 1);
        wl_seat_add_listener(s->seat, &seat_listener, s);
    }
    if (!strcmp(interface, "zwp_input_method_manager_v2"))
        s->manager = wl_registry_bind(registry, name, &zwp_input_method_manager_v2_interface, 1);
}
static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener registry_listener = {.global=global, .global_remove=removed};
static bool exists(const char *directory, const char *name) {
    char path[4096];
    if (snprintf(path, sizeof(path), "%s/%s", directory, name) >= (int)sizeof(path)) exit(2);
    return access(path, F_OK) == 0;
}
static void mark(const char *directory, const char *name) {
    char path[4096];
    if (snprintf(path, sizeof(path), "%s/%s", directory, name) >= (int)sizeof(path)) exit(2);
    FILE *f = fopen(path, "w"); if (!f) {perror("marker"); exit(2);} fputs("ready\n", f); fclose(f);
}
int main(int argc, char **argv) {
    if (argc != 2) {fprintf(stderr, "usage: native-ime-client CONTROL_DIRECTORY\n"); return 2;}
    struct state s = {0};
    s.display = wl_display_connect(NULL);
    if (!s.display) {perror("wl_display_connect"); return 2;}
    struct wl_registry *registry = wl_display_get_registry(s.display);
    wl_registry_add_listener(registry, &registry_listener, &s);
    if (wl_display_roundtrip(s.display) < 0 || !s.seat || !s.manager) {
        fprintf(stderr, "Required seat/input-method-v2 protocol unavailable\n"); return 2;
    }
    s.method = zwp_input_method_manager_v2_get_input_method(s.manager, s.seat);
    zwp_input_method_v2_add_listener(s.method, &method_listener, &s);
    if (wl_display_roundtrip(s.display) < 0 || s.unavailable) return 2;
    mark(argv[1], "connected");
    time_t end = time(NULL) + 30;
    bool preedit_sent = false, committed = false;
    while (time(NULL) < end && !s.unavailable) {
        if (s.active && !preedit_sent && exists(argv[1], "start")) {
            zwp_input_method_v2_set_preedit_string(s.method, "ni", 2, 2);
            zwp_input_method_v2_commit(s.method, s.serial);
            if (wl_display_roundtrip(s.display) < 0) return 2;
            preedit_sent = true; mark(argv[1], "preedit"); puts("preedit sent"); fflush(stdout);
        }
        if (s.active && preedit_sent && !committed && exists(argv[1], "commit")) {
            zwp_input_method_v2_set_preedit_string(s.method, "", 0, 0);
            zwp_input_method_v2_commit_string(s.method, "你");
            zwp_input_method_v2_commit(s.method, s.serial);
            if (wl_display_roundtrip(s.display) < 0) return 2;
            committed = true; mark(argv[1], "committed"); puts("commit sent"); fflush(stdout);
        }
        if (committed && exists(argv[1], "finish")) {
            zwp_input_method_v2_destroy(s.method);
            zwp_input_method_manager_v2_destroy(s.manager);
            wl_registry_destroy(registry); wl_display_disconnect(s.display); return 0;
        }
        if (wl_display_dispatch_pending(s.display) < 0) return 2;
        if (wl_display_flush(s.display) < 0 && errno != EAGAIN) return 2;
        struct pollfd fd = {.fd=wl_display_get_fd(s.display), .events=POLLIN};
        int ready = poll(&fd, 1, 50);
        if (ready < 0 && errno != EINTR) return 2;
        if (ready > 0 && wl_display_dispatch(s.display) < 0) return 2;
    }
    fprintf(stderr, "IME fixture timed out or became unavailable\n"); return 1;
}
