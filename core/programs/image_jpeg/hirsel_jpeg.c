#include "native.h"

#include <limits.h>
#include <setjmp.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>

#define JPEG_INTERNALS
#include "jpeglib.h"
#include "jerror.h"
#include "jmemsys.h"

HIRSEL_ABI_TEXT

enum {
    INVALID = -1,
    LIMIT = -2,
    SHORT_BUFFER = -3,
    BAD_STATE = -4,
    STRIP_ROWS = 16,
    IO_WINDOW = 16384,
    MAX_MARKER = 65536
};

struct codec {
    struct jpeg_error_mgr error;
    jmp_buf jump;
    size_t left;
    int status;
};

static void reject(j_common_ptr jpeg, int status)
{
    struct codec *codec = jpeg->client_data;
    codec->status = status;
    longjmp(codec->jump, 1);
}

static void jpeg_error(j_common_ptr jpeg)
{
    switch (jpeg->err->msg_code) {
    case JERR_NO_BACKING_STORE:
    case JERR_OUT_OF_MEMORY:
        reject(jpeg, LIMIT);
        break;
    default:
        reject(jpeg, INVALID);
    }
}

static void jpeg_message(j_common_ptr jpeg, int level)
{
    if (level < 0)
        reject(jpeg, INVALID);
}

static void setup(j_common_ptr jpeg, struct codec *codec, size_t limit)
{
    jpeg->client_data = codec;
    jpeg->err = jpeg_std_error(&codec->error);
    codec->error.error_exit = jpeg_error;
    codec->error.emit_message = jpeg_message;
    codec->left = limit;
}

/* Provide libjpeg's platform allocator. The upstream jmemnobs.o is then
 * excluded by archive extraction. There is no backing store. */
void *jpeg_get_small(j_common_ptr jpeg, size_t size)
{
    struct codec *codec = jpeg->client_data;
    if (size > codec->left)
        return NULL;
    void *p = malloc(size);
    if (p)
        codec->left -= size;
    return p;
}

void jpeg_free_small(j_common_ptr jpeg, void *p, size_t size)
{
    struct codec *codec = jpeg->client_data;
    codec->left += size;
    free(p);
}

void *jpeg_get_large(j_common_ptr jpeg, size_t size)
{
    return jpeg_get_small(jpeg, size);
}

void jpeg_free_large(j_common_ptr jpeg, void *p, size_t size)
{
    jpeg_free_small(jpeg, p, size);
}

size_t jpeg_mem_available(j_common_ptr jpeg, size_t minimum, size_t maximum, size_t allocated)
{
    (void)minimum;
    (void)allocated;
    struct codec *codec = jpeg->client_data;
    return codec->left < maximum ? codec->left : maximum;
}

void jpeg_open_backing_store(j_common_ptr jpeg, backing_store_ptr store, long size)
{
    (void)store;
    (void)size;
    reject(jpeg, LIMIT);
}

long jpeg_mem_init(j_common_ptr jpeg)
{
    (void)jpeg;
    return 0;
}

void jpeg_mem_term(j_common_ptr jpeg)
{
    (void)jpeg;
}

enum decode_phase { HEADER, SCANS, PIXELS, TRAILER, DONE, FAILED };

struct decoder {
    struct jpeg_decompress_struct jpeg;
    struct codec codec;
    struct jpeg_source_mgr source;
    size_t offset;
    size_t end;
    size_t skipped;
    size_t window;
    bool at_eof;
    enum decode_phase phase;
};

static void source_noop(j_decompress_ptr jpeg)
{
    (void)jpeg;
}

static boolean suspend_source(j_decompress_ptr jpeg)
{
    struct decoder *d = (struct decoder *)jpeg;
    if (d->at_eof)
        reject((j_common_ptr)jpeg, INVALID);
    return FALSE;
}

static void skip_source(j_decompress_ptr jpeg, long bytes)
{
    struct decoder *d = (struct decoder *)jpeg;
    if (bytes <= 0)
        return;
    size_t available = jpeg->src->bytes_in_buffer;
    size_t skip = (size_t)bytes;
    if (skip > available) {
        d->skipped = skip - available;
        skip = available;
    }
    jpeg->src->next_input_byte += skip;
    jpeg->src->bytes_in_buffer -= skip;
}

static void source_attach(struct decoder *d, const uint8_t *input, size_t size)
{
    size_t remaining = size - d->offset;
    size_t count = remaining < d->window ? remaining : d->window;
    d->end = d->offset + count;
    d->at_eof = d->end == size;
    d->skipped = 0;
    d->source.next_input_byte = input + d->offset;
    d->source.bytes_in_buffer = count;
}

static int32_t source_detach(struct decoder *d, size_t size, int32_t status)
{
    size_t offset = d->end - d->source.bytes_in_buffer;
    d->source.next_input_byte = NULL;
    d->source.bytes_in_buffer = 0;
    if (status < 0 || d->skipped > size - offset) {
        d->phase = FAILED;
        return status < 0 ? status : INVALID;
    }
    offset += d->skipped;
    if (status == 0 && offset == d->offset && d->end < size) {
        /* libjpeg can restart a marker or MCU at the same input offset. */
        if (d->window < MAX_MARKER)
            d->window *= 2;
        else {
            d->phase = FAILED;
            return LIMIT;
        }
    } else {
        d->window = IO_WINDOW;
    }
    d->offset = offset;
    return status;
}

void *hjpg_decoder_create(uint64_t memory)
{
    if (memory < sizeof(struct decoder))
        return NULL;
    struct decoder *d = calloc(1, sizeof(*d));
    if (!d)
        return NULL;
    setup((j_common_ptr)&d->jpeg, &d->codec, memory - sizeof(*d));
    if (setjmp(d->codec.jump)) {
        hjpg_decoder_destroy(d);
        return NULL;
    }
    jpeg_create_decompress(&d->jpeg);
    d->source.init_source = source_noop;
    d->source.fill_input_buffer = suspend_source;
    d->source.skip_input_data = skip_source;
    d->source.resync_to_restart = jpeg_resync_to_restart;
    d->source.term_source = source_noop;
    d->jpeg.src = &d->source;
    d->window = IO_WINDOW;
    return d;
}

void hjpg_decoder_destroy(void *decoder)
{
    struct decoder *d = decoder;
    jpeg_destroy_decompress(&d->jpeg);
    free(d);
}

/* 0 means retry, 1 means the dimensions are available. */
int32_t hjpg_decoder_start(void *decoder, const uint8_t *input, uint64_t size, uint32_t scale)
{
    struct decoder *d = decoder;
    if (d->phase != HEADER || size < d->offset || scale < 1 || scale > 8)
        return BAD_STATE;

    source_attach(d, input, size);
    if (setjmp(d->codec.jump))
        return source_detach(d, size, d->codec.status);

    if (jpeg_read_header(&d->jpeg, TRUE) == JPEG_SUSPENDED)
        return source_detach(d, size, 0);
    d->jpeg.out_color_space = JCS_RGB;
    d->jpeg.scale_num = scale;
    d->jpeg.scale_denom = 8;
    d->jpeg.buffered_image = jpeg_has_multiple_scans(&d->jpeg);
    jpeg_start_decompress(&d->jpeg);
    d->phase = d->jpeg.buffered_image ? SCANS : PIXELS;
    return source_detach(d, size, 1);
}

uint32_t hjpg_decoder_width(void *decoder)
{
    return ((struct decoder *)decoder)->jpeg.output_width;
}

uint32_t hjpg_decoder_height(void *decoder)
{
    return ((struct decoder *)decoder)->jpeg.output_height;
}

/* A positive value is a strip's row count. Zero is pending. -5 is EOF. */
int32_t hjpg_decode_strip(void *decoder, const uint8_t *input, uint64_t size,
                          uint8_t *rgb, uint64_t rgb_size)
{
    struct decoder *d = decoder;
    if (d->phase == DONE)
        return -5;
    if (d->phase == HEADER || d->phase == FAILED || size < d->offset)
        return BAD_STATE;

    size_t stride = 3u * d->jpeg.output_width;
    size_t rows = d->jpeg.output_height - d->jpeg.output_scanline;
    if (rows > STRIP_ROWS)
        rows = STRIP_ROWS;
    if (rgb_size < rows * stride)
        return SHORT_BUFFER;

    source_attach(d, input, size);
    if (setjmp(d->codec.jump))
        return source_detach(d, size, d->codec.status);

    int32_t result = 0;
    if (d->phase == SCANS) {
        int step = jpeg_consume_input(&d->jpeg);
        result = step != JPEG_SUSPENDED;
        if (step == JPEG_REACHED_EOI) {
            jpeg_start_output(&d->jpeg, d->jpeg.input_scan_number);
            d->phase = PIXELS;
        }
    } else if (d->phase == PIXELS) {
        JSAMPROW row[STRIP_ROWS];
        for (size_t i = 0; i < rows; ++i)
            row[i] = rgb + i * stride;
        result = (int32_t)jpeg_read_scanlines(&d->jpeg, row, (JDIMENSION)rows);
        if (d->jpeg.output_scanline == d->jpeg.output_height)
            d->phase = TRAILER;
        return source_detach(d, size, result);
    } else {
        if (d->jpeg.buffered_image && d->jpeg.global_state == DSTATE_SCANNING)
            jpeg_finish_output(&d->jpeg);
        if (jpeg_finish_decompress(&d->jpeg)) {
            d->phase = DONE;
            /* Commit the cursor before returning the non-error EOF sentinel. */
            int32_t detached = source_detach(d, size, 1);
            return detached < 0 ? detached : -5;
        }
    }
    int32_t detached = source_detach(d, size, result);
    return detached < 0 ? detached : 0;
}

enum encode_phase { NEW_ENCODER, WRITING, FINISHED, BROKEN };

struct encoder {
    struct jpeg_compress_struct jpeg;
    struct codec codec;
    struct jpeg_destination_mgr dest;
    size_t written;
    size_t end;
    bool suspend;
    bool progressive;
    enum encode_phase phase;
};

static void dest_noop(j_compress_ptr jpeg)
{
    (void)jpeg;
}

static boolean empty_dest(j_compress_ptr jpeg)
{
    struct encoder *e = (struct encoder *)jpeg;
    if (!e->suspend)
        reject((j_common_ptr)jpeg, SHORT_BUFFER);
    return FALSE;
}

static void dest_attach(struct encoder *e, uint8_t *output, size_t size, bool suspend)
{
    size_t remaining = size - e->written;
    if (suspend && remaining > IO_WINDOW)
        remaining = IO_WINDOW;
    e->end = e->written + remaining;
    e->suspend = suspend;
    e->dest.next_output_byte = output + e->written;
    e->dest.free_in_buffer = remaining;
}

static void dest_detach(struct encoder *e)
{
    e->written = e->end - e->dest.free_in_buffer;
    e->dest.next_output_byte = NULL;
    e->dest.free_in_buffer = 0;
}

static int dest_fail(struct encoder *e)
{
    dest_detach(e);
    e->phase = BROKEN;
    return e->codec.status;
}

void *hjpg_encoder_create(uint64_t memory)
{
    if (memory < sizeof(struct encoder))
        return NULL;
    struct encoder *e = calloc(1, sizeof(*e));
    if (!e)
        return NULL;
    setup((j_common_ptr)&e->jpeg, &e->codec, memory - sizeof(*e));
    if (setjmp(e->codec.jump)) {
        hjpg_encoder_destroy(e);
        return NULL;
    }
    jpeg_create_compress(&e->jpeg);
    e->dest.init_destination = dest_noop;
    e->dest.empty_output_buffer = empty_dest;
    e->dest.term_destination = dest_noop;
    e->jpeg.dest = &e->dest;
    return e;
}

void hjpg_encoder_destroy(void *encoder)
{
    struct encoder *e = encoder;
    jpeg_destroy_compress(&e->jpeg);
    free(e);
}

int32_t hjpg_encoder_start(void *encoder, uint32_t width, uint32_t height, int32_t quality,
                           uint32_t mode, uint8_t *output, uint64_t size)
{
    struct encoder *e = encoder;
    if (e->phase != NEW_ENCODER)
        return BAD_STATE;
    if (!width || width > JPEG_MAX_DIMENSION || !height || height > JPEG_MAX_DIMENSION ||
        quality < 1 || quality > 100 || mode > 3)
        return INVALID;
    if (size < 1024)
        return SHORT_BUFFER;

    dest_attach(e, output, size, false);
    if (setjmp(e->codec.jump))
        return dest_fail(e);
    e->jpeg.image_width = width;
    e->jpeg.image_height = height;
    e->jpeg.input_components = 3;
    e->jpeg.in_color_space = JCS_RGB;
    jpeg_set_defaults(&e->jpeg);
    jpeg_set_quality(&e->jpeg, quality, TRUE);
    if ((mode & 1u) == 0) {
        e->jpeg.comp_info[0].h_samp_factor = 1;
        e->jpeg.comp_info[0].v_samp_factor = 1;
    }
    e->progressive = (mode & 2u) != 0;
    if (e->progressive)
        jpeg_simple_progression(&e->jpeg);
    jpeg_start_compress(&e->jpeg, TRUE);
    dest_detach(e);
    e->phase = WRITING;
    return 0;
}

int32_t hjpg_encode_strip(void *encoder, const uint8_t *rgb, uint64_t rgb_size,
                          uint8_t *output, uint64_t size)
{
    struct encoder *e = encoder;
    if (e->phase != WRITING || e->jpeg.next_scanline == e->jpeg.image_height)
        return BAD_STATE;
    size_t stride = 3u * e->jpeg.image_width;
    size_t rows = rgb_size / stride;
    if (!rows || rgb_size % stride || rows > STRIP_ROWS ||
        rows > e->jpeg.image_height - e->jpeg.next_scanline)
        return INVALID;
    if (size < e->written || size - e->written < 1024)
        return SHORT_BUFFER;

    dest_attach(e, output, size, !e->progressive);
    if (setjmp(e->codec.jump))
        return dest_fail(e);

    JSAMPROW row[STRIP_ROWS];
    for (size_t i = 0; i < rows; ++i)
        row[i] = (JSAMPROW)(rgb + i * stride);
    size_t prior = e->written;
    JDIMENSION written = jpeg_write_scanlines(&e->jpeg, row, (JDIMENSION)rows);
    dest_detach(e);
    if (written == 0 && e->written == prior)
        return SHORT_BUFFER;
    return (int32_t)written;
}

static int64_t finish(struct encoder *e, uint8_t *output, uint64_t size, bool progressive)
{
    if (e->phase != WRITING || e->progressive != progressive ||
        e->jpeg.next_scanline != e->jpeg.image_height)
        return BAD_STATE;
    if (size <= e->written || size > INT64_MAX)
        return SHORT_BUFFER;

    dest_attach(e, output, size, false);
    if (setjmp(e->codec.jump))
        return dest_fail(e);
    jpeg_finish_compress(&e->jpeg);
    dest_detach(e);
    e->phase = FINISHED;
    return (int64_t)e->written;
}

int64_t hjpg_encode_finish(void *encoder, uint8_t *output, uint64_t size)
{
    return finish(encoder, output, size, false);
}

int64_t hjpg_encode_finish_progressive(void *encoder, uint8_t *output, uint64_t size)
{
    return finish(encoder, output, size, true);
}

int32_t hjpg_decoder_start_waiting(void *decoder, const uint8_t *input, uint64_t size, uint32_t scale)
{
    return hjpg_decoder_start(decoder, input, size, scale);
}

int32_t hjpg_decode_strip_waiting(void *decoder, const uint8_t *input, uint64_t size,
                                 uint8_t *rgb, uint64_t rgb_size)
{
    return hjpg_decode_strip(decoder, input, size, rgb, rgb_size);
}

int32_t hjpg_encoder_start_waiting(void *encoder, uint32_t width, uint32_t height, int32_t quality,
                                  uint32_t mode, uint8_t *output, uint64_t size)
{
    return hjpg_encoder_start(encoder, width, height, quality, mode, output, size);
}

int32_t hjpg_encode_strip_waiting(void *encoder, const uint8_t *rgb, uint64_t rgb_size,
                                 uint8_t *output, uint64_t size)
{
    return hjpg_encode_strip(encoder, rgb, rgb_size, output, size);
}
