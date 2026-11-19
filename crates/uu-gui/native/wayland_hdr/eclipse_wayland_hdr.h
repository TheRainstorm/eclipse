#pragma once

/* The native presenter is used on the viewport's EGL thread. Winit retains
 * ownership of the display, parent surface, event loop and input. */
struct wl_display;
struct wl_surface;
struct eclipse_pq_child;
struct eclipse_composer;

struct eclipse_pq_child *eclipse_pq_open(struct wl_display *display, struct wl_surface *parent);
/* -1 unavailable, 0 awaiting asynchronous output evidence, 1 HDR10 output. */
int eclipse_pq_output_status(struct eclipse_pq_child *child);
/* True after hardware/compositor presentation on the current output. */
int eclipse_pq_has_presented(struct eclipse_pq_child *child);
/* -1 permanent failure, 0 paint the ordinary window, 1 paint the HDR target,
 * 2 yield and check again when the event loop redraws. */
int eclipse_pq_admit_gpu(struct eclipse_pq_child *child, int vsync, int width, int height);
int eclipse_pq_submit_gpu(struct eclipse_pq_child *child, struct eclipse_composer *scene,
                          float reference_white_nits, int width, int height, int logical_width,
                          int logical_height);
void eclipse_pq_hide(struct eclipse_pq_child *child);
void eclipse_pq_close(struct eclipse_pq_child *child);

struct eclipse_composer *eclipse_compose_new(void);
int eclipse_compose_begin(struct eclipse_composer *scene, int width, int height);
void eclipse_compose_finish(struct eclipse_composer *scene);
void eclipse_compose_free(struct eclipse_composer *scene);
/* A negative interval leaves the existing parent EGL swap interval alone. */
int eclipse_compose_swap(int interval);
