#include "native.h"

#include <string.h>
#include <webp/decode.h>
#include <webp/demux.h>
#include <webp/encode.h>

HIRSEL_ABI_TEXT

enum {
    WRONG_IMAGE_KIND = -1,
    BUFFER_TOO_SMALL = -2,
    OUT_OF_MEMORY = -3,
    CODEC_FAILURE = -4
};

int32_t hwebp_decode(const uint8_t *input_ptr, uint64_t input_len,
                     uint32_t width, uint32_t height, uint32_t x, uint32_t y,
                     uint32_t crop_width, uint32_t crop_height,
                     uint32_t out_width, uint32_t out_height,
                     uint8_t *rgba_ptr, uint64_t rgba_len)
{
    WebPDecoderConfig config;
    if (!WebPInitDecoderConfig(&config) ||
        WebPGetFeatures(input_ptr, input_len, &config.input) != VP8_STATUS_OK)
        return CODEC_FAILURE;
    if (config.input.has_animation)
        return WRONG_IMAGE_KIND;
    if ((uint32_t)config.input.width != width || (uint32_t)config.input.height != height)
        return CODEC_FAILURE;

    config.options.use_cropping = x || y || crop_width != width || crop_height != height;
    config.options.crop_left = (int)x;
    config.options.crop_top = (int)y;
    config.options.crop_width = (int)crop_width;
    config.options.crop_height = (int)crop_height;
    config.options.use_scaling = out_width != crop_width || out_height != crop_height;
    config.options.scaled_width = (int)out_width;
    config.options.scaled_height = (int)out_height;
    config.output.colorspace = MODE_RGBA;
    config.output.is_external_memory = 1;
    config.output.u.RGBA.rgba = rgba_ptr;
    config.output.u.RGBA.stride = (int)(4 * out_width);
    config.output.u.RGBA.size = rgba_len;

    const VP8StatusCode status = WebPDecode(input_ptr, input_len, &config);
    WebPFreeDecBuffer(&config.output);
    if (status == VP8_STATUS_OUT_OF_MEMORY)
        return OUT_OF_MEMORY;
    return status == VP8_STATUS_OK ? 0 : CODEC_FAILURE;
}

int32_t hwebp_decode_animation(const uint8_t *input_ptr, uint64_t input_len,
                               uint32_t width, uint32_t height, uint32_t frames,
                               uint8_t *rgba_ptr, uint64_t rgba_len,
                               uint8_t *timestamps_ptr, uint64_t timestamps_len)
{
    const size_t frame_bytes = (size_t)width * height * 4;
    if (rgba_len < frame_bytes * frames || timestamps_len < 4u * frames)
        return BUFFER_TOO_SMALL;

    WebPBitstreamFeatures features;
    if (WebPGetFeatures(input_ptr, input_len, &features) != VP8_STATUS_OK)
        return CODEC_FAILURE;
    if (!features.has_animation)
        return WRONG_IMAGE_KIND;
    if ((uint32_t)features.width != width || (uint32_t)features.height != height)
        return CODEC_FAILURE;

    const WebPData data = {input_ptr, input_len};
    WebPAnimDecoder *decoder = WebPAnimDecoderNew(&data, NULL);
    if (!decoder)
        return CODEC_FAILURE;

    WebPAnimInfo info;
    int32_t status = CODEC_FAILURE;
    // Check the demuxer's count before the first call that decodes pixels.
    if (WebPAnimDecoderGetInfo(decoder, &info) && info.frame_count == frames &&
        info.canvas_width == width && info.canvas_height == height) {
        status = 0;
        for (uint32_t frame = 0; frame < frames; ++frame) {
            uint8_t *canvas;
            int timestamp;
            if (!WebPAnimDecoderGetNext(decoder, &canvas, &timestamp)) {
                status = CODEC_FAILURE;
                break;
            }
            memcpy(rgba_ptr + frame * frame_bytes, canvas, frame_bytes);
            uint32_t end_ms = (uint32_t)timestamp;
            for (unsigned byte = 0; byte < 4; ++byte)
                timestamps_ptr[4 * frame + byte] = (uint8_t)(end_ms >> (8 * byte));
        }
    }
    WebPAnimDecoderDelete(decoder);
    return status;
}

struct output {
    uint8_t *data;
    size_t size;
    size_t written;
};

static int write_output(const uint8_t *data, size_t size, const WebPPicture *picture)
{
    struct output *output = picture->custom_ptr;
    if (size > output->size - output->written)
        return 0;
    memcpy(output->data + output->written, data, size);
    output->written += size;
    return 1;
}

int32_t hwebp_encode(const uint8_t *rgba_ptr, uint64_t rgba_len,
                     uint32_t width, uint32_t height, int32_t quality,
                     uint8_t *output_ptr, uint64_t output_len)
{
    if (rgba_len != (uint64_t)width * height * 4)
        return CODEC_FAILURE;
    if (!output_len)
        return BUFFER_TOO_SMALL;

    WebPConfig config;
    WebPPicture picture;
    const int lossless = quality < 0;
    if (!WebPConfigPreset(&config, WEBP_PRESET_DEFAULT, lossless ? 70.f : (float)quality) ||
        !WebPPictureInit(&picture))
        return CODEC_FAILURE;
    config.lossless = lossless;
    config.exact = lossless;
    picture.use_argb = lossless;
    picture.width = (int)width;
    picture.height = (int)height;
    struct output output = {output_ptr, output_len, 0};
    picture.custom_ptr = &output;
    picture.writer = write_output;

    const int ok = WebPPictureImportRGBA(&picture, rgba_ptr, (int)(4 * width)) &&
                   WebPEncode(&config, &picture);
    const WebPEncodingError error = picture.error_code;
    WebPPictureFree(&picture);
    if (ok)
        return (int32_t)output.written;
    if (error == VP8_ENC_ERROR_BAD_WRITE)
        return BUFFER_TOO_SMALL;
    if (error == VP8_ENC_ERROR_OUT_OF_MEMORY || error == VP8_ENC_ERROR_BITSTREAM_OUT_OF_MEMORY)
        return OUT_OF_MEMORY;
    return CODEC_FAILURE;
}
