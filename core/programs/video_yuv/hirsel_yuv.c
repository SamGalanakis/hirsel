#include "native.h"

#include <libyuv.h>
#include <stddef.h>
#include <string.h>
HIRSEL_ABI_TEXT

enum { I420 = 0, NV12 = 1, P010 = 2 };
enum { RGBA = 0, ABGR = 1 };
enum { DIMENSIONS = -1, BUFFER_SIZE = -2, ALIGNMENT = -3, NATIVE = -4 };

static const struct YuvConstants *const yuv_matrix[] = {
    &kYuvI601Constants, &kYuvH709Constants, &kYuv2020Constants,
};
static const struct YuvConstants *const yvu_matrix[] = {
    &kYvuI601Constants, &kYvuH709Constants, &kYvu2020Constants,
};
static const struct ArgbConstants *const rgb_matrix[2][3] = {
    {&kAbgrI601Constants, &kAbgrH709Constants, &kAbgrU2020Constants},
    {&kRgbaI601Constants, &kRgbaH709Constants, &kRgbaU2020Constants},
};

struct frame {
    int w, h, cw, ch;
    int pixels, chroma;
};

static int frame_layout(uint32_t width, uint32_t height, struct frame *f)
{
    if (!width || !height || width > 8192 || height > 8192 ||
        width * height > 33554432)
        return DIMENSIONS;
    *f = (struct frame){
        .w = (int)width, .h = (int)height,
        .cw = (int)(width + 1) / 2, .ch = (int)(height + 1) / 2,
        .pixels = (int)(width * height),
    };
    f->chroma = f->cw * f->ch;
    return 0;
}

static int yuv_buffer(uint32_t format, struct frame f, const void *p, uint64_t n)
{
    uint64_t size = (uint64_t)(f.pixels + 2 * f.chroma);
    if (n != size * (format == P010 ? 2u : 1u))
        return BUFFER_SIZE;
    if (format == P010 && (uintptr_t)p % _Alignof(uint16_t))
        return ALIGNMENT;
    return 0;
}

/* ld.lld redirects the pinned library's malloc/free to the current loan.
 * There are no callbacks or waits, and each call rewinds its own arena. */
static _Thread_local struct {
    uint8_t *next;
    uint64_t remaining;
    int exhausted;
} arena;

void *__wrap_malloc(size_t size)
{
    if (size > arena.remaining || !arena.next) {
        arena.exhausted = 1;
        return NULL;
    }
    void *p = arena.next;
    arena.next += size;
    arena.remaining -= size;
    return p;
}

void __wrap_free(void *p)
{
    (void)p;
}

static void begin_scratch(uint8_t *ptr, uint64_t size)
{
    arena.next = ptr;
    arena.remaining = size;
    arena.exhausted = 0;
}

static int end_scratch(int status)
{
    int exhausted = arena.exhausted;
    begin_scratch(NULL, 0);
    return exhausted ? BUFFER_SIZE : status ? NATIVE : 0;
}

int32_t hyuv_to_rgb(uint32_t format, uint32_t order, uint32_t matrix,
                    uint32_t width, uint32_t height,
                    const uint8_t *src, uint64_t src_len,
                    uint8_t *dst, uint64_t dst_len)
{
    struct frame f;
    if (format > P010 || order > ABGR || matrix > 2 ||
        frame_layout(width, height, &f))
        return DIMENSIONS;
    int rc = yuv_buffer(format, f, src, src_len);
    if (rc)
        return rc;
    if (dst_len != 4u * (uint64_t)f.pixels)
        return BUFFER_SIZE;

    int stride = f.w * 4;
    const struct YuvConstants *yuv = yuv_matrix[matrix];
    if (format == I420) {
        const uint8_t *u = src + f.pixels;
        const uint8_t *v = u + f.chroma;
        rc = order == RGBA
             ? I420ToARGBMatrix(src, f.w, v, f.cw, u, f.cw, dst, stride,
                                yvu_matrix[matrix], f.w, f.h)
             : I420ToRGBAMatrix(src, f.w, u, f.cw, v, f.cw, dst, stride,
                                yuv, f.w, f.h);
    } else if (format == NV12) {
        const uint8_t *uv = src + f.pixels;
        if (order == RGBA)
            rc = NV21ToARGBMatrix(src, f.w, uv, f.cw * 2, dst, stride,
                                  yvu_matrix[matrix], f.w, f.h);
        else {
            rc = NV12ToARGBMatrix(src, f.w, uv, f.cw * 2, dst, stride,
                                  yuv, f.w, f.h);
            if (!rc)
                rc = ARGBToRGBA(dst, stride, dst, stride, f.w, f.h);
        }
    } else {
        const uint16_t *y = (const uint16_t *)src;
        rc = P010ToARGBMatrix(y, f.w, y + f.pixels, f.cw * 2, dst, stride,
                              yuv, f.w, f.h);
        if (!rc)
            rc = order == RGBA ? ARGBToABGR(dst, stride, dst, stride, f.w, f.h)
                               : ARGBToRGBA(dst, stride, dst, stride, f.w, f.h);
    }
    return rc ? NATIVE : 0;
}

int32_t hyuv_from_rgb(uint32_t format, uint32_t order, uint32_t matrix,
                      uint32_t width, uint32_t height,
                      const uint8_t *src, uint64_t src_len, uint8_t *dst,
                      uint64_t dst_len, uint8_t *work, uint64_t work_len)
{
    struct frame f;
    if (format > P010 || order > ABGR || matrix > 2 ||
        frame_layout(width, height, &f))
        return DIMENSIONS;
    int rc = yuv_buffer(format, f, dst, dst_len);
    if (rc)
        return rc;
    if (src_len != 4u * (uint64_t)f.pixels)
        return BUFFER_SIZE;
    if (format != I420 && work_len < 4u * width + 256)
        return BUFFER_SIZE;

    int stride = f.w * 4;
    const struct ArgbConstants *m = rgb_matrix[order][matrix];
    begin_scratch(work, work_len);
    if (format == I420) {
        uint8_t *u = dst + f.pixels;
        rc = ARGBToI420Matrix(src, stride, dst, f.w, u, f.cw, u + f.chroma,
                              f.cw, m, f.w, f.h);
    } else if (format == NV12) {
        rc = ARGBToNV12Matrix(src, stride, dst, f.w, dst + f.pixels,
                              f.cw * 2, m, f.w, f.h);
    } else {
        uint16_t *y = (uint16_t *)dst;
        rc = ARGBToP010Matrix(src, stride, y, f.w, y + f.pixels, f.cw * 2,
                              m, f.w, f.h);
    }
    return end_scratch(rc);
}

int32_t hyuv_scale(uint32_t format, uint32_t filter, uint32_t width,
                   uint32_t height, uint32_t out_width, uint32_t out_height,
                   const uint8_t *src, uint64_t src_len, uint8_t *dst,
                   uint64_t dst_len, uint8_t *work, uint64_t work_len)
{
    struct frame f, d;
    if (format > P010 || filter > kFilterBox ||
        frame_layout(width, height, &f) || frame_layout(out_width, out_height, &d))
        return DIMENSIONS;
    int rc = yuv_buffer(format, f, src, src_len);
    if (!rc)
        rc = yuv_buffer(format, d, dst, dst_len);
    if (rc)
        return rc;

    uint64_t needed = 32u * (width + out_width) + 2048;
    if (format == P010 && (width != out_width || height != out_height))
        needed += src_len + 4u * (uint64_t)d.chroma;
    if (work_len < needed)
        return BUFFER_SIZE;

    begin_scratch(work, work_len);
    if (format == I420) {
        const uint8_t *u = src + f.pixels;
        uint8_t *du = dst + d.pixels;
        rc = I420Scale(src, f.w, u, f.cw, u + f.chroma, f.cw, f.w, f.h,
                       dst, d.w, du, d.cw, du + d.chroma, d.cw, d.w, d.h, filter);
    } else if (format == NV12) {
        rc = NV12Scale(src, f.w, src + f.pixels, f.cw * 2, f.w, f.h,
                       dst, d.w, dst + d.pixels, d.cw * 2, d.w, d.h, filter);
    } else {
        const uint16_t *y = (const uint16_t *)src;
        uint16_t *dy = (uint16_t *)dst;
        rc = P010Scale(y, f.w, y + f.pixels, f.cw * 2, f.w, f.h,
                       dy, d.w, dy + d.pixels, d.cw * 2, d.w, d.h, filter);
    }
    return end_scratch(rc);
}

/* Interleaved U/V is one sample pair. Rotate the pair as a unit so that
 * neither 8-bit odd-sized planes nor 10-bit words need a planar copy. */
static void rotate_uv(const uint8_t *src, uint8_t *dst, struct frame f,
                      int sample_bytes, uint32_t rotation)
{
    int first, row, col;
    switch (rotation) {
    case 90:
        first = f.ch - 1;
        row = -1;
        col = f.ch;
        break;
    case 180:
        first = f.chroma - 1;
        row = -f.cw;
        col = -1;
        break;
    default:
        first = f.ch * (f.cw - 1);
        row = 1;
        col = -f.ch;
        break;
    }
    for (int y = 0; y < f.ch; ++y)
        for (int x = 0; x < f.cw; ++x) {
            int out = first + y * row + x * col;
            memcpy(dst + out * sample_bytes,
                   src + (y * f.cw + x) * sample_bytes, (size_t)sample_bytes);
        }
}

int32_t hyuv_rotate(uint32_t format, uint32_t rotation, uint32_t width,
                    uint32_t height, const uint8_t *src, uint64_t src_len,
                    uint8_t *dst, uint64_t dst_len, uint8_t *work, uint64_t work_len)
{
    struct frame f;
    if (format > P010 || (rotation != 90 && rotation != 180 && rotation != 270) ||
        frame_layout(width, height, &f))
        return DIMENSIONS;
    int rc = yuv_buffer(format, f, src, src_len);
    if (!rc)
        rc = yuv_buffer(format, f, dst, dst_len);
    if (rc)
        return rc;
    if (rotation == 180 && work_len < 2u * width + 192)
        return BUFFER_SIZE;

    int dw = rotation == 180 ? f.w : f.h;
    int dcw = rotation == 180 ? f.cw : f.ch;
    begin_scratch(work, work_len);
    if (format == I420) {
        const uint8_t *u = src + f.pixels;
        uint8_t *du = dst + f.pixels;
        rc = I420Rotate(src, f.w, u, f.cw, u + f.chroma, f.cw, dst, dw,
                        du, dcw, du + f.chroma, dcw, f.w, f.h, rotation);
    } else if (format == NV12) {
        rc = RotatePlane(src, f.w, dst, dw, f.w, f.h, rotation);
        if (!rc)
            rotate_uv(src + f.pixels, dst + f.pixels, f, 2, rotation);
    } else {
        rc = RotatePlane_16((const uint16_t *)src, f.w, (uint16_t *)dst, dw,
                             f.w, f.h, rotation);
        if (!rc)
            rotate_uv(src + 2 * f.pixels, dst + 2 * f.pixels, f, 4, rotation);
    }
    return end_scratch(rc);
}
