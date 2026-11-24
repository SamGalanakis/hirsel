/* Direct libyuv oracle. Pack the source and expected bytes in separate files
 * so each loan starts at an aligned allocation. */
#include <libyuv.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { I420, NV12, P010 };

static const struct YuvConstants *const matrices[] = {
    &kYuvI601Constants, &kYuvH709Constants, &kYuv2020Constants,
};
static const struct ArgbConstants *const rgb_matrices[] = {
    &kArgbI601Constants, &kArgbH709Constants, &kArgbU2020Constants,
};

struct frame {
    int w, h, cw, ch, y, c;
};

static struct frame frame(int w, int h)
{
    int cw = (w + 1) / 2, ch = (h + 1) / 2;
    return (struct frame){ w, h, cw, ch, w * h, cw * ch };
}

static void ok(int rc)
{
    if (rc) {
        fprintf(stderr, "reference failed: %d\n", rc);
        exit(1);
    }
}

static void save(const char *label, int n, const void *p, size_t len)
{
    char name[64];
    snprintf(name, sizeof(name), "%s%d", label, n);
    FILE *file = fopen(name, "wb");
    if (!file)
        exit(1);
    if (fwrite(p, 1, len, file) != len || fclose(file))
        exit(1);
}

/* libyuv's conversion and rotation tests generate frame samples with
 * fastrand(). Keep its recurrence and use a fixed seed for these fixtures. */
static unsigned next(uint32_t *state)
{
    *state = *state * 214013u + 2531011u;
    return *state >> 16;
}

static void packed_rgb(const uint8_t *bgra, uint8_t *dst, int pixels, int abgr)
{
    for (int i = 0; i < pixels; ++i, bgra += 4, dst += 4) {
        dst[0] = bgra[abgr ? 3 : 2];
        dst[1] = bgra[abgr ? 0 : 1];
        dst[2] = bgra[abgr ? 1 : 0];
        dst[3] = bgra[abgr ? 2 : 3];
    }
}

static size_t size(struct frame f, int format)
{
    return (size_t)(f.y + 2 * f.c) * (format == P010 ? 2u : 1u);
}

static void decode(int format, int matrix, struct frame f, const uint8_t *src,
                   uint8_t *bgra)
{
    const struct YuvConstants *m = matrices[matrix];
    const uint8_t *u = src + f.y;
    if (format == I420)
        ok(I420ToARGBMatrix(src, f.w, u, f.cw, u + f.c, f.cw, bgra,
                            f.w * 4, m, f.w, f.h));
    else if (format == NV12)
        ok(NV12ToARGBMatrix(src, f.w, u, f.cw * 2, bgra, f.w * 4, m, f.w, f.h));
    else {
        const uint16_t *y = (const uint16_t *)src;
        ok(P010ToARGBMatrix(y, f.w, y + f.y, f.cw * 2, bgra, f.w * 4, m, f.w, f.h));
    }
}

static void encode(int format, int matrix, struct frame f, const uint8_t *bgra,
                   uint8_t *dst)
{
    const struct ArgbConstants *m = rgb_matrices[matrix];
    uint8_t *u = dst + f.y;
    if (format == I420)
        ok(ARGBToI420Matrix(bgra, f.w * 4, dst, f.w, u, f.cw, u + f.c, f.cw,
                            m, f.w, f.h));
    else if (format == NV12)
        ok(ARGBToNV12Matrix(bgra, f.w * 4, dst, f.w, u, f.cw * 2, m, f.w, f.h));
    else {
        uint16_t *y = (uint16_t *)dst;
        ok(ARGBToP010Matrix(bgra, f.w * 4, y, f.w, y + f.y, f.cw * 2, m, f.w, f.h));
    }
}

static void scale(int format, enum FilterMode mode, struct frame f, struct frame d,
                  const uint8_t *src, uint8_t *dst)
{
    const uint8_t *u = src + f.y;
    uint8_t *du = dst + d.y;
    if (format == I420)
        ok(I420Scale(src, f.w, u, f.cw, u + f.c, f.cw, f.w, f.h,
                     dst, d.w, du, d.cw, du + d.c, d.cw, d.w, d.h, mode));
    else if (format == NV12)
        ok(NV12Scale(src, f.w, u, f.cw * 2, f.w, f.h, dst, d.w, du, d.cw * 2,
                     d.w, d.h, mode));
    else {
        const uint16_t *y = (const uint16_t *)src;
        uint16_t *dy = (uint16_t *)dst;
        ok(P010Scale(y, f.w, y + f.y, f.cw * 2, f.w, f.h, dy, d.w, dy + d.y,
                     d.cw * 2, d.w, d.h, mode));
    }
}

static void rotate(int format, enum RotationMode mode, struct frame f,
                   const uint8_t *src, uint8_t *dst, uint16_t *work)
{
    struct frame d = mode == kRotate180 ? f : frame(f.h, f.w);
    const uint8_t *u = src + f.y;
    uint8_t *du = dst + d.y;
    if (format == I420)
        ok(I420Rotate(src, f.w, u, f.cw, u + f.c, f.cw, dst, d.w,
                      du, d.cw, du + d.c, d.cw, f.w, f.h, mode));
    else if (format == NV12) {
        uint8_t *planar = (uint8_t *)work;
        ok(NV12ToI420Rotate(src, f.w, u, f.cw * 2, dst, d.w, planar, d.cw,
                            planar + d.c, d.cw, f.w, f.h, mode));
        MergeUVPlane(planar, d.cw, planar + d.c, d.cw, du, d.cw * 2, d.cw, d.ch);
    } else {
        uint16_t *py = work, *pu = py + f.y, *pv = pu + f.c;
        uint16_t *ry = pv + f.c, *ru = ry + d.y, *rv = ru + d.c;
        const uint16_t *y = (const uint16_t *)src;
        uint16_t *dy = (uint16_t *)dst;
        ok(P010ToI010(y, f.w, y + f.y, f.cw * 2, py, f.w, pu, f.cw,
                      pv, f.cw, f.w, f.h));
        ok(I010Rotate(py, f.w, pu, f.cw, pv, f.cw, ry, d.w, ru, d.cw,
                      rv, d.cw, f.w, f.h, mode));
        ok(I010ToP010(ry, d.w, ru, d.cw, rv, d.cw, dy, d.w, dy + d.y,
                      d.cw * 2, d.w, d.h));
    }
}

static int cases(struct frame f, struct frame d, int index)
{
    size_t rgb = (size_t)f.y * 4;
    size_t samples = (size_t)f.y + 2u * f.c;
    uint8_t *src = malloc(2 * samples), *dst = malloc(rgb);
    uint8_t *bgra = malloc(rgb), *rgb_in = malloc(rgb), *rgb_out = malloc(rgb);
    uint16_t *work = malloc(4 * samples);
    if (!src || !dst || !bgra || !rgb_in || !rgb_out || !work)
        exit(1);

    uint32_t seed = 1;
    for (size_t i = 0; i < rgb; ++i)
        rgb_in[i] = (uint8_t)next(&seed);

    for (int format = I420; format <= P010; ++format) {
        for (size_t i = 0; i < samples; ++i) {
            unsigned value = next(&seed);
            if (format == P010)
                ((uint16_t *)src)[i] = (uint16_t)(value & 0xffc0);
            else
                src[i] = (uint8_t)value;
        }
        for (int abgr = 0; abgr < 2; ++abgr) {
            for (int m = 0; m < 3; ++m) {
                decode(format, m, f, src, bgra);
                packed_rgb(bgra, rgb_out, f.y, abgr);
                save("in", index, src, size(f, format));
                save("out", index++, rgb_out, rgb);

                for (size_t i = 0; i < rgb; i += 4) {
                    const uint8_t *p = rgb_in + i;
                    bgra[i] = p[abgr ? 1 : 2];
                    bgra[i + 1] = p[abgr ? 2 : 1];
                    bgra[i + 2] = p[abgr ? 3 : 0];
                    bgra[i + 3] = p[abgr ? 0 : 3];
                }
                encode(format, m, f, bgra, dst);
                save("in", index, rgb_in, rgb);
                save("out", index++, dst, size(f, format));
            }
        }
        for (int filter = kFilterNone; filter <= kFilterBox; ++filter) {
            scale(format, filter, f, d, src, dst);
            save("in", index, src, size(f, format));
            save("out", index++, dst, size(d, format));
        }
        for (int mode = 90; mode <= 270; mode += 90) {
            rotate(format, mode, f, src, dst, work);
            save("in", index, src, size(f, format));
            save("out", index++, dst, size(f, format));
        }
    }
    free(work);
    free(rgb_out);
    free(rgb_in);
    free(bgra);
    free(dst);
    free(src);
    return index;
}

int main(void)
{
    /* TEST_FACTOR(2) and the odd-size rotation cases, with the 1039-pixel
     * remainder width also exercised by P010ToARGBMatrixFilterOverflow. */
    int cases_written = cases(frame(64, 32), frame(32, 16), 0);
    cases_written = cases(frame(1039, 5), frame(17, 3), cases_written);
    printf("%d direct libyuv vectors\n", cases_written);
}
