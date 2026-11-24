// Direct upstream API calls used to prepare the law's expected bytes.
#include <dav1d/dav1d.h>
#include <EbSvtAv1Enc.h>

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(expr) do { if (!(expr)) { fprintf(stderr, "%s\n", #expr); exit(1); } } while (0)

static void word(FILE *out, uint32_t value)
{
    uint8_t bytes[] = {value, value >> 8, value >> 16, value >> 24};
    CHECK(fwrite(bytes, 1, sizeof(bytes), out) == sizeof(bytes));
}

static void decode(FILE *in, FILE *out)
{
    Dav1dSettings settings;
    dav1d_default_settings(&settings);
    settings.n_threads = 1;
    settings.max_frame_delay = 1;
    settings.frame_size_limit = 8847360;
    Dav1dContext *decoder;
    CHECK(dav1d_open(&decoder, &settings) == 0);
    for (;;) {
        uint8_t length[4];
        size_t read = fread(length, 1, sizeof(length), in);
        if (!read)
            break;
        CHECK(read == sizeof(length));
        uint32_t len = length[0] | (uint32_t)length[1] << 8 |
            (uint32_t)length[2] << 16 | (uint32_t)length[3] << 24;
        Dav1dData data = {0};
        uint8_t *packet = dav1d_data_create(&data, len);
        CHECK(packet && fread(packet, 1, len, in) == len);
        CHECK(dav1d_send_data(decoder, &data) == 0);

        for (;;) {
            Dav1dPicture pic = {0};
            int rc = dav1d_get_picture(decoder, &pic);
            if (rc == DAV1D_ERR(EAGAIN))
                break;
            CHECK(rc == 0);
            const unsigned w = pic.p.w, h = pic.p.h;
            const unsigned format = pic.p.layout;
            word(out, w);
            word(out, h);
            word(out, pic.p.bpc);
            word(out, format);
            const unsigned bytes = pic.p.bpc > 8 ? 2 : 1;
            for (unsigned plane = 0; plane < (format == DAV1D_PIXEL_LAYOUT_I400 ? 1 : 3); ++plane) {
                const unsigned pw = plane && format != DAV1D_PIXEL_LAYOUT_I444 ? (w + 1) / 2 : w;
                const unsigned ph = plane && format == DAV1D_PIXEL_LAYOUT_I420 ? (h + 1) / 2 : h;
                for (unsigned y = 0; y < ph; ++y) {
                    const uint8_t *row = (const uint8_t *)pic.data[plane] + y * pic.stride[plane != 0];
                    CHECK(fwrite(row, bytes, pw, out) == pw);
                }
            }
            dav1d_picture_unref(&pic);
        }
    }
    dav1d_close(&decoder);
    CHECK(feof(in));
}

static void packets(EbComponentType *enc, FILE *out, bool finished)
{
    for (;;) {
        EbBufferHeaderType *packet = NULL;
        EbErrorType rc = svt_av1_enc_get_packet(enc, &packet, finished);
        if (rc == EB_NoErrorEmptyQueue) {
            CHECK(!finished);
            return;
        }
        CHECK(rc == EB_ErrorNone);
        const bool eos = (packet->flags & EB_BUFFERFLAG_EOS) != 0;
        if (packet->n_filled_len)
            CHECK(fwrite(packet->p_buffer, 1, packet->n_filled_len, out) == packet->n_filled_len);
        svt_av1_enc_release_out_buffer(&packet);
        if (eos)
            return;
    }
}

static void encode(FILE *in, FILE *out, uint32_t avif, uint8_t speed, uint8_t fast, uint32_t depth)
{
    EbComponentType *enc = NULL;
    EbSvtAv1EncConfiguration cfg = {0};
    CHECK(svt_av1_enc_init_handle(&enc, &cfg) == EB_ErrorNone);
    cfg.source_width = 128;
    cfg.source_height = 128;
    cfg.encoder_bit_depth = depth;
    cfg.frame_rate_numerator = 30;
    cfg.frame_rate_denominator = 1;
    cfg.enc_mode = speed;
    cfg.qp = 30;
    cfg.rate_control_mode = SVT_AV1_RC_MODE_CQP_OR_CRF;
    cfg.avif = avif != 0;
    cfg.fast_decode = fast;
    cfg.level_of_parallelism = 1;
    CHECK(svt_av1_enc_set_parameter(enc, &cfg) == EB_ErrorNone);
    CHECK(svt_av1_enc_init(enc) == EB_ErrorNone);

    const size_t y = 128 * 128 * (depth > 8 ? 2 : 1);
    const size_t size = y * 3 / 2;
    uint8_t *input = malloc(size);
    CHECK(input);
    EbSvtIOFormat frame = { .luma = input, .cb = input + y, .cr = input + y * 5 / 4,
                            .y_stride = 128, .cb_stride = 64, .cr_stride = 64 };
    EbBufferHeaderType picture = { .size = sizeof(picture), .p_buffer = (uint8_t *)&frame,
                                   .n_filled_len = size, .pic_type = EB_AV1_INVALID_PICTURE };
    for (size_t read; (read = fread(input, 1, size, in)) != 0; ++picture.pts) {
        CHECK(read == size);
        CHECK(svt_av1_enc_send_picture(enc, &picture) == EB_ErrorNone);
        packets(enc, out, false);
    }
    CHECK(feof(in));
    free(input);
    EbBufferHeaderType eos = { .size = sizeof(eos), .flags = EB_BUFFERFLAG_EOS,
                               .pic_type = EB_AV1_INVALID_PICTURE };
    CHECK(svt_av1_enc_send_picture(enc, &eos) == EB_ErrorNone);
    packets(enc, out, true);
    CHECK(svt_av1_enc_deinit(enc) == EB_ErrorNone);
    CHECK(svt_av1_enc_deinit_handle(enc) == EB_ErrorNone);
}

int main(int argc, char **argv)
{
    CHECK(argc == 4 || argc == 8);
    FILE *in = fopen(argv[2], "rb");
    FILE *out = fopen(argv[3], "wb");
    CHECK(in && out);
    if (argc == 4 && strcmp(argv[1], "decode") == 0) {
        decode(in, out);
    } else {
        CHECK(argc == 8 && strcmp(argv[1], "encode") == 0);
        encode(in, out, atoi(argv[4]), atoi(argv[5]), atoi(argv[6]), atoi(argv[7]));
    }
    CHECK(fclose(in) == 0 && fclose(out) == 0);
    return 0;
}
