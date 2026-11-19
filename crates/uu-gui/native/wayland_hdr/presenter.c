#define _GNU_SOURCE
#include <stdbool.h>
#include <stdio.h>
#include "eclipse_wayland_hdr.h"
#include "gpu_buffer.h"
#include "presentation-time.h"
#include "color-management-v1.h"
#include "viewporter.h"
struct output_binding {
    struct output_binding *next;
    uint32_t name;
};

struct receipt {
    struct receipt *next;
    struct eclipse_pq_child *owner;
    struct wp_presentation_feedback *feedback;
    uint64_t generation;
    bool saw_output;
};
struct eclipse_pq_child {
    struct output_binding *outputs;
    uint32_t selected_proxy;
    struct wp_color_management_output_v1 *output_color;
    struct wp_image_description_v1 *output_image;
    struct wp_image_description_info_v1 *output_info;
    struct wp_presentation_feedback *parent_feedback;
    uint64_t generation;
    bool confirmed_visible, output_known, parent_saw_output;
    int opaque_width, opaque_height;
    int output_pq, output_bt2020, output_hdr;
    struct wl_display *d, *wrapper;
    struct wl_surface *parent, *surface;
    struct wl_event_queue *q;
    struct wl_registry *registry;
    struct wl_callback *marker;
    struct wl_callback *next_frame;
    int pace_child;
    struct wl_compositor *co;
    struct wl_subcompositor *subco;
    struct wl_subsurface *sub;
    struct zwp_linux_dmabuf_v1 *dmabuf;
    struct gpu gpu;
    struct slot *active_slot;
    struct wp_viewporter *vp;
    struct wp_viewport *viewport;
    struct wp_color_manager_v1 *cm;
    struct wp_color_management_surface_v1 *color;

    struct wp_image_description_v1 *source;

    uint32_t globals[6];
    int pass, fatal, bt2020, pq, param, intent, source_ready;
    int mapped;
    struct slot slots[3];
    struct wp_presentation *presentation;
    struct receipt *waiting;
    unsigned wait_count;
};
/* A window's preferred working space and its monitor's signal may differ.
   Ask wp_presentation which output actually displayed the surface, then read
   that output's description. Winit owns the parent's listeners and commits. */
static void reset_output_description(struct eclipse_pq_child *c) {
    if (c->output_info)
        wp_image_description_info_v1_destroy(c->output_info);
    if (c->output_image)
        wp_image_description_v1_destroy(c->output_image);
    c->output_info = NULL;
    c->output_image = NULL;
    c->output_known = false;
    c->confirmed_visible = false;
    c->generation++;
    c->output_pq = c->output_bt2020 = c->output_hdr = 0;
}
static int output_info_event(const void *data, void *target, uint32_t op, const struct wl_message *msg,
                             union wl_argument *a) {
    struct eclipse_pq_child *c = (void *)data;
    if (!strcmp(msg->name, "icc_file")) {
        close(a[0].h);
        return 0;
    }
    if (target != c->output_info)
        return 0;
    if (!strcmp(msg->name, "primaries_named"))
        c->output_bt2020 = a[0].u == WP_COLOR_MANAGER_V1_PRIMARIES_BT2020;
    if (!strcmp(msg->name, "primaries")) {
        static const int32_t bt2020[] = {708000, 292000, 170000, 797000, 131000, 46000, 312700, 329000};
        bool match = true;
        for (unsigned i = 0; i < 8; i++)
            if (llabs((long long)a[i].i - bt2020[i]) > 1)
                match = false;
        c->output_bt2020 |= match;
    }
    if (!strcmp(msg->name, "tf_named"))
        c->output_pq = a[0].u == WP_COLOR_MANAGER_V1_TRANSFER_FUNCTION_ST2084_PQ;
    if (!strcmp(msg->name, "done")) {
        c->output_known = true;
        c->output_hdr = c->output_pq && c->output_bt2020;

        wp_image_description_info_v1_destroy(c->output_info);
        c->output_info = NULL;
    }
    return 0;
}
static void output_fail(void *data, struct wp_image_description_v1 *image, uint32_t cause,
                        const char *message) {
    struct eclipse_pq_child *c = data;
    if (image == c->output_image) {
        c->output_known = true;
        c->output_hdr = 0;
        c->confirmed_visible = false;
    }
}
static void output_ready(void *data, struct wp_image_description_v1 *image, uint32_t id) {
    struct eclipse_pq_child *c = data;
    if (image != c->output_image)
        return;
    c->output_info = wp_image_description_v1_get_information(image);
    if (c->output_info)
        wl_proxy_add_dispatcher((struct wl_proxy *)c->output_info, output_info_event, c, c);
}
static const struct wp_image_description_v1_listener output_desc = {.failed = output_fail,
                                                                    .ready = output_ready};
static void refresh_output(void *data, struct wp_color_management_output_v1 *target) {
    struct eclipse_pq_child *c = data;
    if (c->output_color != target)
        return;
    reset_output_description(c);
    c->output_image = wp_color_management_output_v1_get_image_description(target);
    if (c->output_image)
        wp_image_description_v1_add_listener(c->output_image, &output_desc, c);
}
static const struct wp_color_management_output_v1_listener color_events = {.image_description_changed =
                                                                               refresh_output};
/* Use the output resource that Winit already bound. Another wl_output binding on
   the same connection would also be announced in wl_surface.enter to Winit. */
static void select_output(struct eclipse_pq_child *c, struct wl_output *output) {
    uint32_t id = output ? wl_proxy_get_id((struct wl_proxy *)output) : 0;
    if (c->selected_proxy == id)
        return;
    reset_output_description(c);
    if (c->output_color)
        wp_color_management_output_v1_destroy(c->output_color);
    c->output_color = NULL;
    c->selected_proxy = id;
    if (!output || !c->cm)
        return;
    c->output_color = wp_color_manager_v1_get_output(c->cm, output);
    if (c->output_color) {
        wp_color_management_output_v1_add_listener(c->output_color, &color_events, c);
        refresh_output(c, c->output_color);
    }
}
static void register_output(struct eclipse_pq_child *c, uint32_t name) {
    struct output_binding *b = calloc(1, sizeof *b);
    if (b) {
        b->name = name;
        b->next = c->outputs;
        c->outputs = b;
    }
}
static void unregister_output(struct eclipse_pq_child *c, uint32_t name) {
    struct output_binding **link = &c->outputs;
    while (*link && (*link)->name != name)
        link = &(*link)->next;
    if (!*link)
        return;
    struct output_binding *b = *link;
    *link = b->next;
    free(b);
    /* Registry global names and client-side resource IDs are different.
       Confirm the selected output again at the next presentation. */
    select_output(c, NULL);
}
static void parent_output(void *data, struct wp_presentation_feedback *fb, struct wl_output *output) {
    struct eclipse_pq_child *c = data;
    if (!c->mapped && !c->parent_saw_output) {
        c->parent_saw_output = true;
        select_output(c, output);
    }
}
static void parent_done(void *data, struct wp_presentation_feedback *fb) {
    struct eclipse_pq_child *c = data;
    if (c->parent_feedback == fb)
        c->parent_feedback = NULL;
    wp_presentation_feedback_destroy(fb);
}
static void parent_shown(void *data, struct wp_presentation_feedback *fb, uint32_t hi, uint32_t lo,
                         uint32_t ns, uint32_t refresh, uint32_t sh, uint32_t sl, uint32_t flags) {
    struct eclipse_pq_child *c = data;
    if (!c->mapped && !c->parent_saw_output)
        select_output(c, NULL);
    parent_done(data, fb);
}
static const struct wp_presentation_feedback_listener parent_events = {
    .sync_output = parent_output, .presented = parent_shown, .discarded = parent_done};
static void observe_parent(struct eclipse_pq_child *c) {
    if (!c->presentation || !c->cm || c->mapped || c->parent_feedback)
        return;
    c->parent_saw_output = false;
    c->parent_feedback = wp_presentation_feedback(c->presentation, c->parent);
    if (c->parent_feedback)
        wp_presentation_feedback_add_listener(c->parent_feedback, &parent_events, c);
}

static void finish_receipt(struct receipt *r) {
    struct eclipse_pq_child *c = r->owner;
    struct receipt **link = &c->waiting;
    while (*link && *link != r)
        link = &(*link)->next;
    if (*link) {
        *link = r->next;
        c->wait_count--;
    }
    wp_presentation_feedback_destroy(r->feedback);
    free(r);
}
/* Every sync_output for one feedback denotes the same physical output,
 * even if a toolkit bound several wl_output resources for it. Use one of
 * those borrowed resources only while dispatching this event. */
static void sync_output(void *data, struct wp_presentation_feedback *feedback,
                        struct wl_output *output) {
    struct receipt *r = data;
    if (!r->saw_output && r->owner->mapped && r->generation == r->owner->generation)
        select_output(r->owner, output);
    r->saw_output = true;
}
static void presented(void *data, struct wp_presentation_feedback *feedback, uint32_t hi, uint32_t lo,
                      uint32_t nsec, uint32_t refresh, uint32_t seq_hi, uint32_t seq_lo,
                      uint32_t flags) {
    struct receipt *r = data;
    struct eclipse_pq_child *c = r->owner;
    if (c->mapped && r->generation == c->generation) {
        if (!r->saw_output)
            select_output(c, NULL);
        else if (c->output_hdr)
            c->confirmed_visible = true;
    }
    finish_receipt(r);
}
static void discarded(void *data, struct wp_presentation_feedback *feedback) { finish_receipt(data); }
static const struct wp_presentation_feedback_listener receipt_events = {
    .sync_output = sync_output, .presented = presented, .discarded = discarded};
static void track_commit(struct eclipse_pq_child *c) {
    if (c->wait_count >= 8)
        return;
    struct receipt *r = calloc(1, sizeof(*r));
    if (!r)
        return;
    r->feedback = wp_presentation_feedback(c->presentation, c->surface);
    if (!r->feedback) {
        free(r);
        return;
    }
    r->owner = c;
    r->generation = c->generation;
    r->next = c->waiting;
    c->waiting = r;
    c->wait_count++;
    wp_presentation_feedback_add_listener(r->feedback, &receipt_events, r);
}
static void frame_ready(void *data, struct wl_callback *callback, uint32_t ignored) {
    (void)ignored;
    struct eclipse_pq_child *c = data;
    if (callback == c->next_frame)
        c->next_frame = NULL;
    wl_callback_destroy(callback);
}
static const struct wl_callback_listener frame_listener = {.done = frame_ready};

static void cancel_frame(struct eclipse_pq_child *c) {
    if (c && c->next_frame) {
        wl_callback_destroy(c->next_frame);
        c->next_frame = NULL;
    }
}
void eclipse_pq_hide(struct eclipse_pq_child *c) {
    cancel_frame(c);
    if (c && c->mapped) {
        wl_surface_attach(c->surface, NULL, 0, 0);
        wl_surface_commit(c->surface);
        c->mapped = 0;
        c->confirmed_visible = false;
        c->generation++;
        wl_display_flush(c->d);
    }
}
static void source_failed(void *data, struct wp_image_description_v1 *image, uint32_t cause,
                          const char *message) {
    struct eclipse_pq_child *c = data;
    c->fatal = 1;
    eclipse_pq_hide(c);
}
static void source_ready(void *data, struct wp_image_description_v1 *image, uint32_t id) {
    struct eclipse_pq_child *c = data;
    c->source_ready = 1;
}
static const struct wp_image_description_v1_listener desc_events = {.failed = source_failed,
                                                                    .ready = source_ready};
static void old_format(void *d, struct zwp_linux_dmabuf_v1 *b, uint32_t f) {}
static void modifier(void *d, struct zwp_linux_dmabuf_v1 *b, uint32_t format, uint32_t hi, uint32_t lo) {
    struct eclipse_pq_child *c = d;
    compositor_modifier(&c->gpu, format, ((uint64_t)hi << 32) | lo);
}
static const struct zwp_linux_dmabuf_v1_listener dmabuf_events = {.format = old_format,
                                                                  .modifier = modifier};
static void intent(void *d, struct wp_color_manager_v1 *m, uint32_t v) {
    struct eclipse_pq_child *c = d;
    c->intent |= v == WP_COLOR_MANAGER_V1_RENDER_INTENT_PERCEPTUAL;
}
static void feature(void *d, struct wp_color_manager_v1 *m, uint32_t v) {
    struct eclipse_pq_child *c = d;
    c->param |= v == WP_COLOR_MANAGER_V1_FEATURE_PARAMETRIC;
}
static void tf(void *d, struct wp_color_manager_v1 *m, uint32_t v) {
    struct eclipse_pq_child *c = d;
    c->pq |= v == WP_COLOR_MANAGER_V1_TRANSFER_FUNCTION_ST2084_PQ;
}
static void prim(void *d, struct wp_color_manager_v1 *m, uint32_t v) {
    struct eclipse_pq_child *c = d;
    c->bt2020 |= v == WP_COLOR_MANAGER_V1_PRIMARIES_BT2020;
}
static void done(void *d, struct wp_color_manager_v1 *m) {}
static const struct wp_color_manager_v1_listener capability = {.supported_intent = intent,
                                                               .supported_feature = feature,
                                                               .supported_tf_named = tf,
                                                               .supported_primaries_named = prim,
                                                               .done = done};

static void removed(void *d, struct wl_registry *r, uint32_t id) {
    struct eclipse_pq_child *c = d;
    unregister_output(c, id);
    for (int i = 0; i < 6; i++)
        if (c->globals[i] == id)
            c->fatal = 1;
}
static void global(void *d, struct wl_registry *r, uint32_t name, const char *iface, uint32_t version) {
    struct eclipse_pq_child *c = d;
    if (!strcmp(iface, wl_output_interface.name)) {
        register_output(c, name);
        return;
    }
    /* All objects created from this registry inherit our private event queue. */
    if (!strcmp(iface, wp_presentation_interface.name) && !c->presentation) {
        c->globals[5] = name;
        c->presentation = wl_registry_bind(r, name, &wp_presentation_interface, 1);
    } else if (!strcmp(iface, wl_compositor_interface.name) && !c->co) {
        c->globals[0] = name;
        c->co = wl_registry_bind(r, name, &wl_compositor_interface, 1);
    } else if (!strcmp(iface, wl_subcompositor_interface.name) && !c->subco) {
        c->globals[1] = name;
        c->subco = wl_registry_bind(r, name, &wl_subcompositor_interface, 1);
    } else if (!strcmp(iface, zwp_linux_dmabuf_v1_interface.name) && !c->dmabuf && version >= 3) {
        c->globals[2] = name;
        c->dmabuf = wl_registry_bind(r, name, &zwp_linux_dmabuf_v1_interface, 3);
        if (c->dmabuf)
            zwp_linux_dmabuf_v1_add_listener(c->dmabuf, &dmabuf_events, c);
    } else if (!strcmp(iface, wp_viewporter_interface.name) && !c->vp) {
        c->globals[3] = name;
        c->vp = wl_registry_bind(r, name, &wp_viewporter_interface, 1);
    } else if (!strcmp(iface, wp_color_manager_v1_interface.name) && !c->cm) {
        c->globals[4] = name;
        c->cm = wl_registry_bind(r, name, &wp_color_manager_v1_interface, 1);
        if (c->cm)
            wp_color_manager_v1_add_listener(c->cm, &capability, c);
    }
}
static const struct wl_registry_listener registry_events = {.global = global, .global_remove = removed};
static void marker_done(void *d, struct wl_callback *cb, uint32_t serial);
static const struct wl_callback_listener marker_event = {.done = marker_done};
static void barrier(struct eclipse_pq_child *c) {
    c->marker = wl_display_sync(c->wrapper);
    if (c->marker)
        wl_callback_add_listener(c->marker, &marker_event, c);
    else
        c->fatal = 1;
}
static void marker_done(void *d, struct wl_callback *cb, uint32_t serial) {
    struct eclipse_pq_child *c = d;
    wl_callback_destroy(cb);
    c->marker = NULL;
    if (c->fatal)
        return;
    if (!c->pass++) {
        barrier(c);
        return;
    }
    if (!c->presentation || !c->cm || !c->co || !c->subco || !c->vp || !c->dmabuf || !c->gpu.count ||
        !c->pq || !c->param || !c->bt2020 || !c->intent) {
        c->fatal = 1;
        return;
    }
    struct wp_image_description_creator_params_v1 *p =
        wp_color_manager_v1_create_parametric_creator(c->cm);
    if (!p) {
        c->fatal = 1;
        return;
    }
    wp_image_description_creator_params_v1_set_primaries_named(p, WP_COLOR_MANAGER_V1_PRIMARIES_BT2020);
    wp_image_description_creator_params_v1_set_tf_named(p,
                                                        WP_COLOR_MANAGER_V1_TRANSFER_FUNCTION_ST2084_PQ);
    c->source = wp_image_description_creator_params_v1_create(p);

    if (!c->source) {
        c->fatal = 1;
        return;
    }
    wp_image_description_v1_add_listener(c->source, &desc_events, c);
}
struct eclipse_pq_child *eclipse_pq_open(struct wl_display *d, struct wl_surface *parent) {
    if (!d || !parent)
        return NULL;
    struct eclipse_pq_child *c = calloc(1, sizeof(*c));
    if (!c)
        return NULL;
    c->q = wl_display_create_queue(d);
    if (!c->q) {
        free(c);
        return NULL;
    }
    c->d = d;
    c->parent = parent;
    c->wrapper = wl_proxy_create_wrapper(d);
    if (!c->wrapper) {
        eclipse_pq_close(c);
        return NULL;
    }
    wl_proxy_set_queue((struct wl_proxy *)c->wrapper, c->q);
    c->registry = wl_display_get_registry(c->wrapper);
    if (!c->registry) {
        eclipse_pq_close(c);
        return NULL;
    }
    wl_registry_add_listener(c->registry, &registry_events, c);
    barrier(c);
    wl_display_flush(d);
    return c;
}
int eclipse_pq_output_status(struct eclipse_pq_child *c) {
    if (!c)
        return -1;
    if (wl_display_get_error(c->d) || wl_display_dispatch_queue_pending(c->d, c->q) < 0)
        c->fatal = 1;
    if (!c->fatal)
        observe_parent(c);
    if (wl_display_flush(c->d) < 0 && errno != EAGAIN)
        c->fatal = 1;
    if (c->fatal || !c->output_hdr)
        eclipse_pq_hide(c);
    if (c->fatal || (c->output_known && !c->output_hdr))
        return -1;
    return c->source_ready && c->output_hdr;
}
int eclipse_pq_has_presented(struct eclipse_pq_child *c) {
    /* The event owner dispatches at prepare/negotiation. HUD reads must never
       dispatch or flush Wayland from a secondary status query. */
    return c && !c->fatal && c->mapped && c->output_hdr && c->confirmed_visible;
}
/* -1 permanent failure, 0 ordinary SDR paint, 1 native paint, 2 yield. */
int eclipse_pq_admit_gpu(struct eclipse_pq_child *c, int vsync, int w, int h) {
    int ready = eclipse_pq_output_status(c);
    if (ready <= 0)
        return c && !c->fatal ? 0 : -1;
    c->pace_child = !!vsync;
    if (!c->pace_child)
        cancel_frame(c);
    if (c->next_frame)
        return 2;
    if (w <= 0 || h <= 0)
        return 0;
    c->active_slot = NULL;
    if (!gpu_init(&c->gpu)) {
        c->fatal = 1;
        return -1;
    }
    for (int i = 0; i < 3; i++) {
        struct slot *s = &c->slots[i];
        if (s->failed) {
            if (exclude_slot_layout(&c->gpu, s))
                return 2;
            c->fatal = 1;
            return -1;
        }
        if (s->params && now_ms() - s->requested_ms > 2000) {
            c->fatal = 1;
            return -1;
        }
        if (s->busy || s->params)
            continue;
        if (s->buffer && s->width == (unsigned)w && s->height == (unsigned)h) {
            c->active_slot = s;
            return 1;
        }
    }
    for (int i = 0; i < 3; i++) {
        struct slot *s = &c->slots[i];
        if (s->params && s->width == (unsigned)w && s->height == (unsigned)h)
            return 2;
    }
    for (int i = 0; i < 3; i++) {
        struct slot *s = &c->slots[i];
        if (s->busy || s->params)
            continue;
        slot_drop(&c->gpu, s);
        if (!slot_allocate(&c->gpu, s, c->dmabuf, w, h)) {
            if (exclude_slot_layout(&c->gpu, s))
                return 2;
            c->fatal = 1;
            return -1;
        }
        wl_display_flush(c->d);
        break;
    }
    return 2;
}
static int make_surface(struct eclipse_pq_child *c) {
    c->surface = wl_compositor_create_surface(c->co);
    if (!c->surface)
        return 0;
    c->sub = wl_subcompositor_get_subsurface(c->subco, c->surface, c->parent);
    c->viewport = wp_viewporter_get_viewport(c->vp, c->surface);
    c->color = wp_color_manager_v1_get_surface(c->cm, c->surface);
    if (!c->sub || !c->viewport || !c->color)
        return 0;
    struct wl_region *r = wl_compositor_create_region(c->co);
    if (!r)
        return 0;
    wl_surface_set_input_region(c->surface, r);
    wl_region_destroy(r);
    wl_subsurface_set_desync(c->sub);
    wp_color_management_surface_v1_set_image_description(c->color, c->source,
                                                         WP_COLOR_MANAGER_V1_RENDER_INTENT_PERCEPTUAL);
    return 1;
}
int eclipse_compose_gpu(struct eclipse_composer *, float, GLuint);
int eclipse_pq_submit_gpu(struct eclipse_pq_child *c, struct eclipse_composer *composer, float white,
                          int w, int h, int logical_w, int logical_h) {
    int ready = eclipse_pq_output_status(c);
    if (ready <= 0)
        return c && !c->fatal ? 0 : -1;
    if (!composer || logical_w < 1 || logical_h < 1)
        return -1;
    struct slot *s = c->active_slot;
    c->active_slot = NULL;
    if (!s || s->busy || !s->buffer || s->width != (unsigned)w || s->height != (unsigned)h)
        return -1;
    if ((!c->surface && !make_surface(c)) || !order_previous_readers(&c->gpu, s) ||
        !eclipse_compose_gpu(composer, white, s->fbo) || !publish_writes(&c->gpu, s)) {
        c->fatal = 1;
        eclipse_pq_hide(c);
        return -1;
    }
    if (c->pace_child) {
        if (c->next_frame)
            return 0;
        c->next_frame = wl_surface_frame(c->surface);
        if (!c->next_frame) {
            c->fatal = 1;
            eclipse_pq_hide(c);
            return -1;
        }
        wl_callback_add_listener(c->next_frame, &frame_listener, c);
    }
    s->busy = 1;
    s->used = 1;
    /* All video, cursor and GUI layers have been composed. The final
       RGB10 shader always writes alpha=1, including the letterbox. */
    if (c->opaque_width != logical_w || c->opaque_height != logical_h) {
        struct wl_region *opaque = wl_compositor_create_region(c->co);
        if (opaque) {
            wl_region_add(opaque, 0, 0, logical_w, logical_h);
            wl_surface_set_opaque_region(c->surface, opaque);
            wl_region_destroy(opaque);
            c->opaque_width = logical_w;
            c->opaque_height = logical_h;
        }
    }
    wp_viewport_set_destination(c->viewport, logical_w, logical_h);
    wl_surface_attach(c->surface, s->buffer, 0, 0);
    wl_surface_damage(c->surface, 0, 0, logical_w, logical_h);
    track_commit(c);
    wl_surface_commit(c->surface);
    c->mapped = 1;
    if (wl_display_flush(c->d) < 0 && errno != EAGAIN) {
        c->fatal = 1;
        return -1;
    }
    return 1;
}
void eclipse_pq_close(struct eclipse_pq_child *c) {
    if (!c)
        return;
    eclipse_pq_hide(c);
    while (c->waiting)
        finish_receipt(c->waiting);
    if (c->parent_feedback)
        wp_presentation_feedback_destroy(c->parent_feedback);
    while (c->outputs)
        unregister_output(c, c->outputs->name);
    if (c->presentation)
        wp_presentation_destroy(c->presentation);
    if (c->marker)
        wl_callback_destroy(c->marker);
    select_output(c, NULL);
    if (c->source)
        wp_image_description_v1_destroy(c->source);
    if (c->viewport)
        wp_viewport_destroy(c->viewport);
    if (c->color)
        wp_color_management_surface_v1_destroy(c->color);
    if (c->sub)
        wl_subsurface_destroy(c->sub);
    if (c->surface)
        wl_surface_destroy(c->surface);
    for (int i = 0; i < 3; i++)
        slot_drop(&c->gpu, &c->slots[i]);
    if (c->gpu.device) {
        gbm_device_destroy(c->gpu.device);
        close(c->gpu.fd);
    }
    if (c->cm)
        wp_color_manager_v1_destroy(c->cm);
    if (c->vp)
        wp_viewporter_destroy(c->vp);
    if (c->dmabuf)
        zwp_linux_dmabuf_v1_destroy(c->dmabuf);
    if (c->subco)
        wl_subcompositor_destroy(c->subco);
    if (c->co)
        wl_compositor_destroy(c->co);
    if (c->registry)
        wl_registry_destroy(c->registry);
    if (c->wrapper)
        wl_proxy_wrapper_destroy(c->wrapper);
    if (c->q)
        wl_event_queue_destroy(c->q);
    if (c->d)
        wl_display_flush(c->d);

    free(c);
}
