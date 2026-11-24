#include <stdio.h>
#include <stdlib.h>
#include <webp/decode.h>
#include <webp/demux.h>
#include <webp/encode.h>

static void require(int ok, const char *message)
{
    if (!ok) {
        fprintf(stderr, "%s\n", message);
        exit(1);
    }
}

static void write_animation(const WebPData *input)
{
    WebPAnimDecoder *decoder = WebPAnimDecoderNew(input, NULL);
    WebPAnimInfo info;
    require(decoder && WebPAnimDecoderGetInfo(decoder, &info), "animation header");
    const size_t size = (size_t)info.canvas_width * info.canvas_height * 4;
    for (uint32_t frame = 0; frame < info.frame_count; ++frame) {
        uint8_t *canvas;
        int timestamp;
        require(WebPAnimDecoderGetNext(decoder, &canvas, &timestamp), "animation frame");
        for (unsigned byte = 0; byte < 4; ++byte)
            require(putchar(((uint32_t)timestamp >> (8 * byte)) & 255) != EOF, "timestamp write");
        require(fwrite(canvas, 1, size, stdout) == size, "canvas write");
    }
    WebPAnimDecoderDelete(decoder);
}

static void write_still(const WebPData *input, int crop_scale)
{
    WebPDecoderConfig config;
    require(WebPInitDecoderConfig(&config), "decoder config");
    config.output.colorspace = MODE_RGBA;
    if (crop_scale) {
        config.options.use_cropping = 1;
        config.options.crop_left = 2;
        config.options.crop_top = 4;
        config.options.crop_width = 96;
        config.options.crop_height = 80;
        config.options.use_scaling = 1;
        config.options.scaled_width = 37;
        config.options.scaled_height = 29;
    }
    require(WebPDecode(input->bytes, input->size, &config) == VP8_STATUS_OK, "still image decode");
    const size_t size = (size_t)config.output.width * config.output.height * 4;
    require(fwrite(config.output.u.RGBA.rgba, 1, size, stdout) == size, "pixel write");
    WebPFreeDecBuffer(&config.output);
}

static void write_encoded(const WebPData *input, int width, int height, int lossless)
{
    require(input->size == (size_t)width * height * 4, "wrong RGBA size");
    WebPConfig config;
    WebPPicture picture;
    require(WebPConfigPreset(&config, WEBP_PRESET_DEFAULT, lossless ? 70.f : 82.f) &&
            WebPPictureInit(&picture), "encoder config");
    WebPMemoryWriter writer;
    WebPMemoryWriterInit(&writer);
    config.lossless = lossless;
    config.exact = lossless;
    picture.use_argb = lossless;
    picture.width = width;
    picture.height = height;
    picture.writer = WebPMemoryWrite;
    picture.custom_ptr = &writer;
    require(WebPPictureImportRGBA(&picture, input->bytes, width * 4) &&
            WebPEncode(&config, &picture), "encode");
    require(fwrite(writer.mem, 1, writer.size, stdout) == writer.size, "encoded write");
    WebPMemoryWriterClear(&writer);
    WebPPictureFree(&picture);
}

int main(int argc, char **argv)
{
    require(argc == 3, "usage: reference MODE INPUT");
    FILE *file = fopen(argv[2], "rb");
    require(file && fseek(file, 0, SEEK_END) == 0, "open input");
    long size = ftell(file);
    require(size > 0 && fseek(file, 0, SEEK_SET) == 0, "input size");
    uint8_t *bytes = malloc((size_t)size);
    require(bytes && fread(bytes, 1, (size_t)size, file) == (size_t)size, "read input");
    require(fclose(file) == 0, "close input");
    const WebPData data = {bytes, (size_t)size};
    switch (argv[1][0]) {
        case 's': write_still(&data, 0); break;
        case 'c': write_still(&data, 1); break;
        case 'a': write_animation(&data); break;
        case 'l': write_encoded(&data, 37, 29, 1); break;
        case 'q': write_encoded(&data, 37, 29, 0); break;
        case 't': write_encoded(&data, 4, 4, 1); break;
        default: require(0, "unknown reference mode");
    }
    free(bytes);
    require(fflush(stdout) == 0, "flush output");
    return 0;
}
