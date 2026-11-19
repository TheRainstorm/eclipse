#define _GNU_SOURCE
#include <epoxy/egl.h>
#include <epoxy/gl.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "pq_shader.h"
#include "eclipse_wayland_hdr.h"

typedef struct eclipse_composer {
    EGLContext context;
    GLuint fbo, texture, program, vao;
    GLint scene_uniform, white_uniform;
    GLint previous_draw_fbo, previous_read_fbo;
    bool painting;
    int width, height;
} Composer;

static GLuint compile(GLenum type, const char *body) {
    GLuint shader = glCreateShader(type);
    const char *version = epoxy_is_desktop_gl()
                              ? "#version 330 core\n"
                              : "#version 300 es\nprecision highp float; precision highp int;\n";
    const char *parts[] = {version, body};
    glShaderSource(shader, 2, parts, NULL);
    glCompileShader(shader);
    GLint ok = 0;
    glGetShaderiv(shader, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char msg[1024];
        glGetShaderInfoLog(shader, sizeof msg, NULL, msg);
        fprintf(stderr, "Eclipse HDR output shader: %s\n", msg);
        glDeleteShader(shader);
        return 0;
    }
    return shader;
}
void eclipse_compose_free(Composer *p) {
    if (!p)
        return;
    if (eglGetCurrentContext() == p->context && p->context != EGL_NO_CONTEXT) {
        eclipse_compose_finish(p);
        glDeleteFramebuffers(1, &p->fbo);
        glDeleteTextures(1, &p->texture);
        glDeleteProgram(p->program);
        glDeleteVertexArrays(1, &p->vao);
    }
    free(p);
}
Composer *eclipse_compose_new(void) {
    EGLContext ctx = eglGetCurrentContext();
    if (ctx == EGL_NO_CONTEXT || epoxy_gl_version() < (epoxy_is_desktop_gl() ? 33 : 30))
        return NULL;
    Composer *p = calloc(1, sizeof *p);
    if (!p)
        return NULL;
    p->context = ctx;
    GLuint v = compile(
        GL_VERTEX_SHADER,
        "void main(){ vec2 p=vec2((gl_VertexID<<1)&2,gl_VertexID&2);gl_Position=vec4(p*2.0-1.0,0,1);}");
    GLuint f = compile(GL_FRAGMENT_SHADER, pq_frag);
    if (v && f) {
        p->program = glCreateProgram();
        glAttachShader(p->program, v);
        glAttachShader(p->program, f);
        glLinkProgram(p->program);
    }
    if (v)
        glDeleteShader(v);
    if (f)
        glDeleteShader(f);
    GLint linked = 0;
    if (p->program)
        glGetProgramiv(p->program, GL_LINK_STATUS, &linked);
    if (!linked) {
        eclipse_compose_free(p);
        return NULL;
    }
    glGenVertexArrays(1, &p->vao);
    glGenFramebuffers(1, &p->fbo);
    glGenTextures(1, &p->texture);
    p->scene_uniform = glGetUniformLocation(p->program, "scene");
    p->white_uniform = glGetUniformLocation(p->program, "reference_white");
    return p;
}
int eclipse_compose_begin(Composer *p, int width, int height) {
    if (!p || p->painting || p->context != eglGetCurrentContext() || width <= 0 || height <= 0 || width > 8192 ||
        height > 8192 || (size_t)width * height > 16777216)
        return 0;
    glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &p->previous_draw_fbo);
    glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &p->previous_read_fbo);
    if (p->width != width || p->height != height) {
        GLint old_texture = 0, old_unpack = 0;
        glGetIntegerv(GL_TEXTURE_BINDING_2D, &old_texture);
        glGetIntegerv(GL_PIXEL_UNPACK_BUFFER_BINDING, &old_unpack);
        glBindBuffer(GL_PIXEL_UNPACK_BUFFER, 0);
        bool ok;

        glBindTexture(GL_TEXTURE_2D, p->texture);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA16F, width, height, 0, GL_RGBA, GL_HALF_FLOAT, NULL);
        glBindFramebuffer(GL_FRAMEBUFFER, p->fbo);
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, p->texture, 0);
        ok = glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE;
        glBindTexture(GL_TEXTURE_2D, old_texture);
        glBindFramebuffer(GL_DRAW_FRAMEBUFFER, p->previous_draw_fbo);
        glBindFramebuffer(GL_READ_FRAMEBUFFER, p->previous_read_fbo);
        glBindBuffer(GL_PIXEL_UNPACK_BUFFER, old_unpack);
        if (!ok)
            return 0;
        p->width = width;
        p->height = height;
    }
    glBindFramebuffer(GL_FRAMEBUFFER, p->fbo);
    p->painting = true;
    return 1;
}
static void enable(GLenum cap, GLboolean on) {
    if (on)
        glEnable(cap);
    else
        glDisable(cap);
}
static GLint get(GLenum what) {
    GLint x;
    glGetIntegerv(what, &x);
    return x;
}
int eclipse_compose_gpu(Composer *p, float ui_white, GLuint target) {
    if (!p || !target || p->context != eglGetCurrentContext() || !(ui_white > 0))
        return 0;
    GLint prog = get(GL_CURRENT_PROGRAM), vao = get(GL_VERTEX_ARRAY_BINDING),
          unit = get(GL_ACTIVE_TEXTURE);
    GLint draw = get(GL_DRAW_FRAMEBUFFER_BINDING), read = get(GL_READ_FRAMEBUFFER_BINDING);
    GLint v[4];
    GLboolean mask[4], blend = glIsEnabled(GL_BLEND), scissor = glIsEnabled(GL_SCISSOR_TEST);
    GLboolean depth = glIsEnabled(GL_DEPTH_TEST), cull = glIsEnabled(GL_CULL_FACE),
              dither = glIsEnabled(GL_DITHER);
    bool desktop = epoxy_is_desktop_gl();
    GLboolean srgb = desktop && glIsEnabled(GL_FRAMEBUFFER_SRGB);
    glGetBooleanv(GL_COLOR_WRITEMASK, mask);
    glGetIntegerv(GL_VIEWPORT, v);
    glActiveTexture(GL_TEXTURE0);
    GLint texture = get(GL_TEXTURE_BINDING_2D), sampler = get(GL_SAMPLER_BINDING);
    glDisable(GL_BLEND);
    glDisable(GL_SCISSOR_TEST);
    glDisable(GL_DEPTH_TEST);
    glDisable(GL_CULL_FACE);
    glDisable(GL_DITHER);
    if (desktop)
        glDisable(GL_FRAMEBUFFER_SRGB);
    glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
    glUseProgram(p->program);
    glBindVertexArray(p->vao);
    glViewport(0, 0, p->width, p->height);
    glBindSampler(0, 0);
    glBindTexture(GL_TEXTURE_2D, p->texture);
    glUniform1i(p->scene_uniform, 0);
    glUniform1f(p->white_uniform, ui_white);
    glBindFramebuffer(GL_FRAMEBUFFER, target);
    glDrawArrays(GL_TRIANGLES, 0, 3);
    GLenum result = glGetError();
    glBindVertexArray(vao);
    glUseProgram(prog);
    glBindTexture(GL_TEXTURE_2D, texture);
    glBindSampler(0, sampler);
    glActiveTexture(unit);
    glBindFramebuffer(GL_DRAW_FRAMEBUFFER, draw);
    glBindFramebuffer(GL_READ_FRAMEBUFFER, read);
    glViewport(v[0], v[1], v[2], v[3]);
    glColorMask(mask[0], mask[1], mask[2], mask[3]);
    enable(GL_BLEND, blend);
    enable(GL_SCISSOR_TEST, scissor);
    enable(GL_DEPTH_TEST, depth);
    enable(GL_CULL_FACE, cull);
    enable(GL_DITHER, dither);
    if (desktop)
        enable(GL_FRAMEBUFFER_SRGB, srgb);
    return result == GL_NO_ERROR;
}
void eclipse_compose_finish(Composer *p) {
    if (p && p->painting && p->context == eglGetCurrentContext()) {
        glBindFramebuffer(GL_DRAW_FRAMEBUFFER, p->previous_draw_fbo);
        glBindFramebuffer(GL_READ_FRAMEBUFFER, p->previous_read_fbo);
        p->painting = false;
    }
}
int eclipse_compose_swap(int swap_interval) {
    EGLDisplay d = eglGetCurrentDisplay();
    EGLSurface s = eglGetCurrentSurface(EGL_DRAW);
    if (d == EGL_NO_DISPLAY || s == EGL_NO_SURFACE)
        return 0;
    if (swap_interval >= 0 && !eglSwapInterval(d, swap_interval))
        return 0;
    return eglSwapBuffers(d, s) == EGL_TRUE;
}
