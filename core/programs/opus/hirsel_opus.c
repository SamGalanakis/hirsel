#include "native.h"

#include <limits.h>
#include <opus.h>
#include <stddef.h>
HIRSEL_ABI_TEXT

void *hop_encoder_create(int32_t rate, int32_t channels, int32_t application)
{
    return opus_encoder_create(rate, channels, application, NULL);
}

void hop_encoder_destroy(void *encoder)
{
    opus_encoder_destroy(encoder);
}

void *hop_decoder_create(int32_t rate, int32_t channels)
{
    return opus_decoder_create(rate, channels, NULL);
}

void hop_decoder_destroy(void *decoder)
{
    opus_decoder_destroy(decoder);
}

int32_t hop_encode(void *encoder, uint32_t channels,
                   const uint8_t *pcm_ptr, uint64_t pcm_len,
                   uint8_t *packet_ptr, uint64_t packet_len)
{
    unsigned stride = channels * sizeof(opus_int16);
    if ((uintptr_t)pcm_ptr % _Alignof(opus_int16) ||
        !pcm_len || pcm_len > 5760u * stride || pcm_len % stride ||
        packet_len > INT32_MAX)
        return OPUS_BAD_ARG;
    return opus_encode(encoder, (const opus_int16 *)pcm_ptr, (int)(pcm_len / stride),
                       packet_ptr, (opus_int32)packet_len);
}

int32_t hop_decode(void *decoder, uint32_t channels,
                   const uint8_t *packet_ptr, uint64_t packet_len,
                   uint32_t frames, int32_t fec,
                   uint8_t *pcm_ptr, uint64_t pcm_len)
{
    if (!frames || frames > 5760 || packet_len > INT32_MAX ||
        (uintptr_t)pcm_ptr % _Alignof(opus_int16))
        return OPUS_BAD_ARG;
    if (pcm_len < (uint64_t)frames * channels * sizeof(opus_int16))
        return OPUS_BUFFER_TOO_SMALL;
    return opus_decode(decoder, packet_len ? packet_ptr : NULL, (opus_int32)packet_len,
                       (opus_int16 *)pcm_ptr, (int)frames, fec);
}

int32_t hop_set_bitrate(void *encoder, int32_t value)
{
    return opus_encoder_ctl(encoder, OPUS_SET_BITRATE(value));
}

int32_t hop_set_fec(void *encoder, int32_t value)
{
    return opus_encoder_ctl(encoder, OPUS_SET_INBAND_FEC(value));
}

int32_t hop_set_packet_loss_percent(void *encoder, int32_t value)
{
    return opus_encoder_ctl(encoder, OPUS_SET_PACKET_LOSS_PERC(value));
}

int32_t hop_set_dtx(void *encoder, int32_t value)
{
    return opus_encoder_ctl(encoder, OPUS_SET_DTX(value));
}

int32_t hop_set_complexity(void *encoder, int32_t value)
{
    return opus_encoder_ctl(encoder, OPUS_SET_COMPLEXITY(value));
}
