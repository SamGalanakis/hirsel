#include "svt.h"
#include "hirsel_av1.h"

#include <EbSvtAv1Enc.h>
#include <stdlib.h>
#include <string.h>

HIRSEL_ABI_TEXT

typedef struct {
    EbComponentType *context;
    EbBufferHeaderType *packet;
    uint32_t width;
    uint32_t height;
    uint32_t sample_bytes;
    int64_t frames;
    bool avif;
    bool needs_drain;
    bool finished;
    bool failed;
} Encoder;

void *hsvt_create(uint32_t width, uint32_t height, uint32_t depth, uint32_t enc_mode,
                  uint32_t fast_decode, uint32_t level_of_parallelism, uint32_t avif,
                  uint32_t qp, uint32_t fps_num, uint32_t fps_den)
{
    Encoder *encoder = calloc(1, sizeof(*encoder));
    if (!encoder)
        return NULL;

    EbSvtAv1EncConfiguration cfg;
    if (svt_av1_enc_init_handle(&encoder->context, &cfg) != EB_ErrorNone) {
        free(encoder);
        return NULL;
    }
    cfg.source_width = width;
    cfg.source_height = height;
    cfg.encoder_bit_depth = depth;
    cfg.enc_mode = (int8_t)enc_mode;
    cfg.fast_decode = (uint8_t)fast_decode;
    cfg.level_of_parallelism = level_of_parallelism;
    cfg.avif = avif != 0;
    cfg.qp = qp;
    cfg.rate_control_mode = SVT_AV1_RC_MODE_CQP_OR_CRF;
    cfg.frame_rate_numerator = fps_num;
    cfg.frame_rate_denominator = fps_den;

    if (svt_av1_enc_set_parameter(encoder->context, &cfg) != EB_ErrorNone ||
        svt_av1_enc_init(encoder->context) != EB_ErrorNone) {
        svt_av1_enc_deinit(encoder->context);
        svt_av1_enc_deinit_handle(encoder->context);
        free(encoder);
        return NULL;
    }

    encoder->width = width;
    encoder->height = height;
    encoder->sample_bytes = depth > 8 ? 2 : 1;
    encoder->avif = cfg.avif;
    return encoder;
}

void hsvt_destroy(void *handle)
{
    Encoder *encoder = handle;
    if (encoder->packet)
        svt_av1_enc_release_out_buffer(&encoder->packet);
    svt_av1_enc_deinit(encoder->context);
    svt_av1_enc_deinit_handle(encoder->context);
    free(encoder);
}

int32_t hsvt_send(void *handle, const uint8_t *pixels, uint64_t len)
{
    Encoder *encoder = handle;
    if (encoder->failed)
        return AV1_NATIVE;
    if (encoder->finished || (encoder->avif && encoder->frames != 0))
        return AV1_INVALID;
    if (encoder->frames >= INT32_MAX)
        return AV1_LIMIT;
    if (encoder->needs_drain)
        return AV1_DRAIN;

    const uint32_t plane = encoder->width * encoder->height * encoder->sample_bytes;
    if (len != plane + plane / 2 || (uintptr_t)pixels % encoder->sample_bytes)
        return AV1_INVALID;
    EbSvtIOFormat frame = {
        .luma = (uint8_t *)pixels,
        .cb = (uint8_t *)pixels + plane,
        .cr = (uint8_t *)pixels + plane + plane / 4,
        .y_stride = encoder->width,
        .cb_stride = encoder->width / 2,
        .cr_stride = encoder->width / 2,
    };
    EbBufferHeaderType input = {
        .size = sizeof(input),
        .p_buffer = (uint8_t *)&frame,
        .n_filled_len = (uint32_t)len,
        .pts = encoder->frames,
        .pic_type = EB_AV1_INVALID_PICTURE,
    };
    if (svt_av1_enc_send_picture(encoder->context, &input) != EB_ErrorNone) {
        encoder->failed = true;
        return AV1_NATIVE;
    }
    ++encoder->frames;
    encoder->needs_drain = true;
    return 0;
}

int32_t hsvt_finish(void *handle)
{
    Encoder *encoder = handle;
    if (encoder->failed)
        return AV1_NATIVE;
    if (encoder->finished)
        return 0;
    if (encoder->needs_drain)
        return AV1_DRAIN;
    if (encoder->frames != 0) {
        EbBufferHeaderType eos = {
            .size = sizeof(eos),
            .flags = EB_BUFFERFLAG_EOS,
            .pic_type = EB_AV1_INVALID_PICTURE,
        };
        if (svt_av1_enc_send_picture(encoder->context, &eos) != EB_ErrorNone) {
            encoder->failed = true;
            return AV1_NATIVE;
        }
    }
    encoder->finished = true;
    return 0;
}

int64_t hsvt_receive(void *handle, uint8_t *packet, uint64_t len)
{
    Encoder *encoder = handle;
    if (encoder->failed)
        return AV1_NATIVE;
    // A zero return means the queue is empty, so empty packets are skipped.
    while (!encoder->packet || !encoder->packet->n_filled_len) {
        if (encoder->packet)
            svt_av1_enc_release_out_buffer(&encoder->packet);
        encoder->packet = NULL;
        const EbErrorType rc = svt_av1_enc_get_packet(encoder->context, &encoder->packet, 0);
        if (rc == EB_NoErrorEmptyQueue) {
            encoder->packet = NULL;
            encoder->needs_drain = false;
            return 0;
        }
        if (rc != EB_ErrorNone) {
            if (encoder->packet)
                svt_av1_enc_release_out_buffer(&encoder->packet);
            encoder->packet = NULL;
            encoder->failed = true;
            return AV1_NATIVE;
        }
    }

    const uint32_t size = encoder->packet->n_filled_len;
    if (len < size)
        return AV1_BUFFER;
    memcpy(packet, encoder->packet->p_buffer, size);
    svt_av1_enc_release_out_buffer(&encoder->packet);
    encoder->packet = NULL;
    return size;
}
