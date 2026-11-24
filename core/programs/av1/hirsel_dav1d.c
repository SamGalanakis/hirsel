#include "dav.h"
#include "hirsel_av1.h"

#include <dav1d/dav1d.h>
#include <errno.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

HIRSEL_ABI_TEXT

typedef struct {
    Dav1dContext *context;
    Dav1dPicture picture;
    uint32_t max_pixels;
    // Frame size bound of the last sequence header sent, for call-class choice.
    uint64_t sequence_pixels;
    bool needs_drain;
} Decoder;

static int32_t decoder_error(int rc)
{
    switch (rc) {
    case DAV1D_ERR(ERANGE): return AV1_LIMIT;
    case DAV1D_ERR(EINVAL): return AV1_INVALID;
    case DAV1D_ERR(EAGAIN): return AV1_DRAIN;
    default: return AV1_NATIVE;
    }
}

// Checks OBU framing and each sequence header. *sequence_pixels receives the
// largest declared frame size, or stays unchanged without a sequence header.
static int32_t scan(uint32_t max_pixels, const uint8_t *data, uint64_t len,
                    uint64_t *sequence_pixels)
{
    if (len > 16 * 1024 * 1024)
        return AV1_LIMIT;
    if (!len)
        return AV1_INVALID;

    size_t pos = 0;
    unsigned frames = 0;
    while (pos < len) {
        const size_t start = pos;
        const uint8_t header = data[pos++];
        const unsigned type = (header >> 3) & 15;
        if (header & 0x81)
            return AV1_INVALID;
        if (header & 4) {
            if (pos == len)
                return AV1_INVALID;
            ++pos;
        }

        size_t size = len - pos;
        if (header & 2) {
            size = 0;
            for (unsigned shift = 0;; shift += 7) {
                if (pos == len || shift == 56)
                    return AV1_INVALID;
                const uint8_t byte = data[pos++];
                size |= (size_t)(byte & 127) << shift;
                if (!(byte & 128))
                    break;
            }
            if (size > len - pos)
                return AV1_INVALID;
        }
        pos += size;

        if (type == DAV1D_OBU_SEQ_HDR) {
            Dav1dSequenceHeader seq;
            const int rc = dav1d_parse_sequence_header(&seq, data + start, pos - start);
            if (rc < 0)
                return decoder_error(rc);
            const uint64_t pixels = (uint64_t)seq.max_width * seq.max_height;
            if (seq.max_width > 16384 || seq.max_height > 16384 || pixels > max_pixels)
                return AV1_LIMIT;
            *sequence_pixels = pixels;
        }
        if ((type == DAV1D_OBU_FRAME_HDR || type == DAV1D_OBU_FRAME) && ++frames > 32)
            return AV1_LIMIT;
    }
    return 0;
}

int32_t hdav_probe(uint32_t max_pixels, const uint8_t *data, uint64_t len)
{
    uint64_t pixels = 0;
    return scan(max_pixels, data, len, &pixels);
}

int32_t hdav_probe_waiting(uint32_t max_pixels, const uint8_t *data, uint64_t len)
{
    return hdav_probe(max_pixels, data, len);
}

// The frame size a send of these bytes may decode. A packet over 4 KiB, or one
// that does not scan, reports UINT64_MAX so that the caller uses the helper.
uint64_t hdav_send_pixels(void *handle, const uint8_t *data, uint64_t len)
{
    Decoder *decoder = handle;
    uint64_t pixels = decoder->sequence_pixels;
    if (len > 4096 || scan(decoder->max_pixels, data, len, &pixels) < 0)
        return UINT64_MAX;
    return pixels > decoder->sequence_pixels ? pixels : decoder->sequence_pixels;
}

uint64_t hdav_sequence_pixels(void *handle)
{
    return ((Decoder *)handle)->sequence_pixels;
}

void *hdav_create(uint32_t max_pixels)
{
    Decoder *decoder = calloc(1, sizeof(*decoder));
    if (!decoder)
        return NULL;

    Dav1dSettings settings;
    dav1d_default_settings(&settings);
    settings.n_threads = 1;
    settings.max_frame_delay = 1;
    settings.frame_size_limit = max_pixels;
    settings.logger.callback = NULL;
    if (dav1d_open(&decoder->context, &settings) < 0) {
        free(decoder);
        return NULL;
    }
    decoder->max_pixels = max_pixels;
    decoder->sequence_pixels = max_pixels;
    return decoder;
}

void hdav_destroy(void *handle)
{
    Decoder *decoder = handle;
    dav1d_picture_unref(&decoder->picture);
    dav1d_close(&decoder->context);
    free(decoder);
}

void hdav_reset(void *handle)
{
    Decoder *decoder = handle;
    dav1d_picture_unref(&decoder->picture);
    dav1d_flush(decoder->context);
    decoder->needs_drain = false;
}

int32_t hdav_send(void *handle, const uint8_t *data, uint64_t len)
{
    Decoder *decoder = handle;
    if (decoder->needs_drain)
        return AV1_DRAIN;
    uint64_t pixels = decoder->sequence_pixels;
    const int probe = scan(decoder->max_pixels, data, len, &pixels);
    if (probe < 0)
        return probe;
    decoder->sequence_pixels = pixels;

    Dav1dData packet = {0};
    uint8_t *copy = dav1d_data_create(&packet, len);
    if (!copy)
        return AV1_NATIVE;
    memcpy(copy, data, len);

    const int rc = dav1d_send_data(decoder->context, &packet);
    // On success, send_data clears the reference. On error it leaves it with us.
    dav1d_data_unref(&packet);
    decoder->needs_drain = true;
    return rc < 0 ? decoder_error(rc) : 0;
}

int64_t hdav_receive(void *handle, uint8_t *pixels, uint64_t len)
{
    Decoder *decoder = handle;
    Dav1dPicture *p = &decoder->picture;
    if (!p->data[0]) {
        const int rc = dav1d_get_picture(decoder->context, p);
        if (rc == DAV1D_ERR(EAGAIN)) {
            decoder->needs_drain = false;
            return 0;
        }
        if (rc < 0)
            return decoder_error(rc);
    }

    const int width = p->p.w;
    const int height = p->p.h;
    const int bytes = p->p.bpc > 8 ? 2 : 1;
    const bool monochrome = p->p.layout == DAV1D_PIXEL_LAYOUT_I400;
    const int sx = p->p.layout == DAV1D_PIXEL_LAYOUT_I420 || p->p.layout == DAV1D_PIXEL_LAYOUT_I422;
    const int sy = p->p.layout == DAV1D_PIXEL_LAYOUT_I420;
    const int cw = (width + sx) >> sx;
    const int ch = (height + sy) >> sy;
    const size_t needed = ((size_t)width * height + (monochrome ? 0 : 2 * (size_t)cw * ch)) * bytes;
    if (len < needed)
        return AV1_BUFFER;

    for (int plane = 0; plane < (monochrome ? 1 : 3); ++plane) {
        const uint8_t *row = p->data[plane];
        const ptrdiff_t stride = p->stride[plane != 0];
        const size_t row_bytes = (plane == 0 ? width : cw) * bytes;
        const int rows = plane == 0 ? height : ch;
        for (int y = 0; y < rows; ++y) {
            memcpy(pixels, row, row_bytes);
            row += stride;
            pixels += row_bytes;
        }
    }

    const int64_t info = width | ((int64_t)height << 16) |
        ((int64_t)p->p.bpc << 32) | ((int64_t)p->p.layout << 40);
    dav1d_picture_unref(p);
    return info;
}

int32_t hdav_send_waiting(void *handle, const uint8_t *data, uint64_t len)
{
    return hdav_send(handle, data, len);
}

int64_t hdav_receive_waiting(void *handle, uint8_t *pixels, uint64_t len)
{
    return hdav_receive(handle, pixels, len);
}
