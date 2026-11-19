#pragma once
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <limits.h>
#include <errno.h>
#include <string.h>
#include <wayland-client.h>
#include <epoxy/egl.h>
#include <epoxy/gl.h>
#include <gbm.h>
#include <drm_fourcc.h>
#include <linux/dma-buf.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <time.h>
#include "linux-dmabuf.h"
#ifndef EGL_DRM_RENDER_NODE_FILE_EXT
#define EGL_DRM_RENDER_NODE_FILE_EXT 0x3377
#endif
#define MAX_MODS 256
#define PLANES 4
struct gpu {
    EGLDisplay display;
    EGLContext context;
    struct gbm_device *device;
    int fd;
    uint64_t mods[MAX_MODS];
    unsigned count;
};
struct slot {
    struct wl_buffer *buffer;
    struct zwp_linux_buffer_params_v1 *params;
    struct gbm_bo *bo;
    GLuint fbo, texture;
    EGLImageKHR image;
    uint32_t width, height;
    int busy, failed, used, planes, fd[PLANES];
    uint64_t modifier;
    bool has_modifier;
    uint64_t requested_ms;
};
static uint64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}
static void compositor_modifier(struct gpu *g, uint32_t format, uint64_t mod) {
    if (format != DRM_FORMAT_ARGB2101010 || mod == DRM_FORMAT_MOD_INVALID)
        return;
    for (unsigned i = 0; i < g->count; i++)
        if (g->mods[i] == mod)
            return;
    if (g->count < MAX_MODS)
        g->mods[g->count++] = mod;
}
static int gpu_init(struct gpu *g) {
    EGLDisplay d = eglGetCurrentDisplay();
    EGLContext ctx = eglGetCurrentContext();
    if (d == EGL_NO_DISPLAY || ctx == EGL_NO_CONTEXT)
        return 0;
    if (g->device)
        return g->display == d && g->context == ctx;
    if (!epoxy_has_egl_extension(d, "EGL_EXT_image_dma_buf_import_modifiers") ||
        !epoxy_has_egl_extension(d, "EGL_ANDROID_native_fence_sync") ||
        !epoxy_has_egl_extension(d, "EGL_KHR_wait_sync") ||
        !epoxy_has_egl_extension(EGL_NO_DISPLAY, "EGL_EXT_device_query") ||
        !epoxy_has_gl_extension("GL_OES_EGL_image"))
        return 0;
    EGLAttrib dev = 0;
    if (!eglQueryDisplayAttribEXT(d, EGL_DEVICE_EXT, &dev) || !dev)
        return 0;
    const char *device_ext = eglQueryDeviceStringEXT((EGLDeviceEXT)dev, EGL_EXTENSIONS);
    if (!device_ext || !strstr(device_ext, "EGL_EXT_device_drm_render_node"))
        return 0;
    const char *node = eglQueryDeviceStringEXT((EGLDeviceEXT)dev, EGL_DRM_RENDER_NODE_FILE_EXT);
    if (!node)
        return 0;
    int fd = open(node, O_RDWR | O_CLOEXEC);
    if (fd < 0)
        return 0;
    struct stat st;
    if (fstat(fd, &st) || !S_ISCHR(st.st_mode)) {
        close(fd);
        return 0;
    }
    EGLint count = 0;
    if (!eglQueryDmaBufModifiersEXT(d, DRM_FORMAT_ARGB2101010, 0, NULL, NULL, &count) || count < 1 ||
        count > 4096) {
        close(fd);
        return 0;
    }
    EGLuint64KHR *mods = calloc(count, sizeof *mods);
    EGLBoolean *external = calloc(count, sizeof *external);
    EGLint used = 0;
    if (!mods || !external ||
        !eglQueryDmaBufModifiersEXT(d, DRM_FORMAT_ARGB2101010, count, mods, external, &used) ||
        used < 1 || used > count) {
        free(mods);
        free(external);
        close(fd);
        return 0;
    }
    unsigned common = 0;
    for (unsigned i = 0; i < g->count; i++)
        for (int j = 0; j < used; j++)
            if (!external[j] && mods[j] == g->mods[i]) {
                g->mods[common++] = g->mods[i];
                break;
            }
    free(mods);
    free(external);
    g->count = common;
    if (!common) {
        close(fd);
        return 0;
    }
    g->device = gbm_create_device(fd);
    if (!g->device) {
        close(fd);
        return 0;
    }
    g->fd = fd;
    g->context = ctx;
    g->display = d;

    return 1;
}
static void slot_drop(struct gpu *g, struct slot *s) {
    if (s->params)
        zwp_linux_buffer_params_v1_destroy(s->params);
    if (s->buffer)
        wl_buffer_destroy(s->buffer);
    if (s->fbo && eglGetCurrentContext() == g->context)
        glDeleteFramebuffers(1, &s->fbo);
    if (s->texture && eglGetCurrentContext() == g->context)
        glDeleteTextures(1, &s->texture);
    if (s->image)
        eglDestroyImageKHR(g->display, s->image);
    for (int i = 0; i < s->planes; i++)
        close(s->fd[i]);
    if (s->bo)
        gbm_bo_destroy(s->bo);
    memset(s, 0, sizeof *s);
}
static void release(void *data, struct wl_buffer *b) {
    struct slot *s = data;
    if (s->buffer == b)
        s->busy = 0;
}
static const struct wl_buffer_listener released = {.release = release};
static void imported(void *data, struct zwp_linux_buffer_params_v1 *params, struct wl_buffer *b) {
    struct slot *s = data;
    s->buffer = b;
    wl_buffer_add_listener(b, &released, s);
    zwp_linux_buffer_params_v1_destroy(params);
    s->params = NULL;
}
static void import_failed(void *data, struct zwp_linux_buffer_params_v1 *params) {
    struct slot *s = data;
    s->failed = 1;
    zwp_linux_buffer_params_v1_destroy(params);
    s->params = NULL;
}
static const struct zwp_linux_buffer_params_v1_listener import_events = {.created = imported,
                                                                         .failed = import_failed};
static int slot_allocate(struct gpu *g, struct slot *s, struct zwp_linux_dmabuf_v1 *dmabuf, int w,
                         int h) {
    if (w <= 0 || h <= 0 || w > 8192 || h > 8192 || (uint64_t)w * h > 16777216)
        return 0;
    s->bo = gbm_bo_create_with_modifiers2(g->device, w, h, GBM_FORMAT_ARGB2101010, g->mods, g->count,
                                          GBM_BO_USE_RENDERING);
    if (!s->bo)
        return 0;
    uint64_t mod = gbm_bo_get_modifier(s->bo);
    s->has_modifier = true;
    s->modifier = mod;
    unsigned j = 0;
    while (j < g->count && g->mods[j] != mod)
        j++;
    int n = gbm_bo_get_plane_count(s->bo);

    if (j == g->count || n < 1 || n > PLANES || gbm_bo_get_format(s->bo) != GBM_FORMAT_ARGB2101010)
        return 0;
    EGLint a[6 + PLANES * 10 + 1] = {
        EGL_WIDTH, w, EGL_HEIGHT, h, EGL_LINUX_DRM_FOURCC_EXT, DRM_FORMAT_ARGB2101010};
    static const EGLint fd_at[] = {EGL_DMA_BUF_PLANE0_FD_EXT, EGL_DMA_BUF_PLANE1_FD_EXT,
                                   EGL_DMA_BUF_PLANE2_FD_EXT, EGL_DMA_BUF_PLANE3_FD_EXT};
    int pos = 6;
    s->params = zwp_linux_dmabuf_v1_create_params(dmabuf);
    if (!s->params)
        return 0;
    for (int i = 0; i < n; i++) {
        int fd = gbm_bo_get_fd_for_plane(s->bo, i);
        if (fd < 0)
            return 0;
        s->fd[s->planes++] = fd;
        uint32_t stride = gbm_bo_get_stride_for_plane(s->bo, i), off = gbm_bo_get_offset(s->bo, i);
        if (!stride || stride > INT_MAX || off > INT_MAX)
            return 0;
        a[pos++] = fd_at[i];
        a[pos++] = fd;
        a[pos++] = fd_at[i] + 1;
        a[pos++] = off;
        a[pos++] = fd_at[i] + 2;
        a[pos++] = stride;
        a[pos++] = EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT + i * 2;
        a[pos++] = (uint32_t)mod;
        a[pos++] = EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT + i * 2;
        a[pos++] = mod >> 32;
        zwp_linux_buffer_params_v1_add(s->params, fd, i, off, stride, mod >> 32, (uint32_t)mod);
    }
    a[pos++] = EGL_NONE;
    s->image = eglCreateImageKHR(g->display, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, a);
    if (!s->image)
        return 0;
    GLint texture, draw, read;
    glGetIntegerv(GL_TEXTURE_BINDING_2D, &texture);
    glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &draw);
    glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &read);
    glGenTextures(1, &s->texture);
    glBindTexture(GL_TEXTURE_2D, s->texture);
    glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, s->image);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glGenFramebuffers(1, &s->fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, s->fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, s->texture, 0);
    GLenum status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    int ok = status == GL_FRAMEBUFFER_COMPLETE;
    const GLenum channels[] = {GL_FRAMEBUFFER_ATTACHMENT_RED_SIZE, GL_FRAMEBUFFER_ATTACHMENT_GREEN_SIZE,
                               GL_FRAMEBUFFER_ATTACHMENT_BLUE_SIZE};
    for (unsigned k = 0; k < 3; k++) {
        GLint bits = 0;
        glGetFramebufferAttachmentParameteriv(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, channels[k], &bits);
        ok &= bits == 10;
    }
    glBindTexture(GL_TEXTURE_2D, texture);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, draw);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, read);
    GLenum err = glGetError();
    if (!ok || err != GL_NO_ERROR)
        return 0;
    s->width = w;
    s->height = h;
    zwp_linux_buffer_params_v1_add_listener(s->params, &import_events, s);
    s->requested_ms = now_ms();
    zwp_linux_buffer_params_v1_create(s->params, w, h, DRM_FORMAT_ARGB2101010, 0);

    return 1;
}
/* A modifier advertised as importable may only be usable for sampling.
 * Retry the driver's remaining EGL/compositor intersection if this particular
 * layout fails FBO validation or the compositor rejects it asynchronously. */
static bool exclude_slot_layout(struct gpu *g, struct slot *s) {
    if (!s->has_modifier)
        return false;
    for (unsigned i = 0; i < g->count; i++) {
        if (g->mods[i] != s->modifier)
            continue;
        g->mods[i] = g->mods[--g->count];
        break;
    }
    slot_drop(g, s);
    return g->count != 0;
}
static int sync_ioctl(int fd, unsigned long request, void *arg) {
    int result;
    do {
        result = ioctl(fd, request, arg);
    } while (result < 0 && errno == EINTR);
    return result;
}
static int order_previous_readers(struct gpu *g, struct slot *s) {
    if (!s->used)
        return 1;
    for (int i = 0; i < s->planes; i++) {
        struct dma_buf_export_sync_file e = {.flags = DMA_BUF_SYNC_RW, .fd = -1};
        if (sync_ioctl(s->fd[i], DMA_BUF_IOCTL_EXPORT_SYNC_FILE, &e) < 0)
            return 0;
        const EGLint attrs[] = {EGL_SYNC_NATIVE_FENCE_FD_ANDROID, e.fd, EGL_NONE};
        EGLSyncKHR f = eglCreateSyncKHR(g->display, EGL_SYNC_NATIVE_FENCE_ANDROID, attrs);
        if (!f) {
            close(e.fd);
            return 0;
        }
        EGLBoolean ok = eglWaitSyncKHR(g->display, f, 0);
        eglDestroySyncKHR(g->display, f);
        if (!ok)
            return 0;
    }
    return 1;
}
static int publish_writes(struct gpu *g, struct slot *s) {
    EGLSyncKHR f = eglCreateSyncKHR(g->display, EGL_SYNC_NATIVE_FENCE_ANDROID, NULL);
    if (!f)
        return 0;
    glFlush();
    int fd = eglDupNativeFenceFDANDROID(g->display, f);
    eglDestroySyncKHR(g->display, f);
    if (fd < 0)
        return 0;
    struct dma_buf_import_sync_file imp = {.flags = DMA_BUF_SYNC_WRITE, .fd = fd};
    int ok = 1;
    for (int i = 0; i < s->planes; i++)
        if (sync_ioctl(s->fd[i], DMA_BUF_IOCTL_IMPORT_SYNC_FILE, &imp) < 0)
            ok = 0;
    close(fd);
    return ok;
}
