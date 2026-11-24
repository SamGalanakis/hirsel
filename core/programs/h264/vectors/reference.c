#include "codec_api.h"

#include <assert.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void emit_frame(FILE *out, SBufferInfo *info, uint8_t *planes[3])
{
    if (!info->iBufferStatus)
        return;
    SSysMEMBuffer layout = info->UsrData.sSystemBuffer;
    uint32_t dimensions[] = {layout.iWidth, layout.iHeight};
    assert(fwrite(dimensions, sizeof(*dimensions), 2, out) == 2);
    assert(fwrite(&info->uiOutYuvTimeStamp, 8, 1, out) == 1);
    for (int plane = 0; plane < 3; ++plane) {
        int chroma = plane != 0;
        size_t width = layout.iWidth >> chroma;
        size_t height = layout.iHeight >> chroma;
        for (size_t row = 0; row < height; ++row)
            assert(fwrite(planes[plane] + row * layout.iStride[chroma], 1, width, out) == width);
    }
}

static void decode(FILE *in, FILE *out)
{
    ISVCDecoder *decoder = NULL;
    assert(WelsCreateDecoder(&decoder) == 0);
    SDecodingParam param = {0};
    param.uiTargetDqLayer = UCHAR_MAX;
    param.eEcActiveIdc = ERROR_CON_DISABLE;
    param.sVideoProperty.eVideoBsType = VIDEO_BITSTREAM_AVC;
    int trace = WELS_LOG_QUIET;
    assert((*decoder)->SetOption(decoder, DECODER_OPTION_TRACE_LEVEL, &trace) == 0);
    assert((*decoder)->Initialize(decoder, &param) == 0);

    uint32_t size;
    uint64_t timestamp = 100;
    while (fread(&size, 4, 1, in) == 1) {
        uint8_t *nal = malloc(size);
        assert(nal && fread(nal, 1, size, in) == size);
        SBufferInfo info = {0};
        info.uiInBsTimeStamp = timestamp++;
        uint8_t *planes[3] = {0};
        assert((*decoder)->DecodeFrame2(decoder, nal, size, planes, &info) == dsErrorFree);
        emit_frame(out, &info, planes);
        free(nal);
    }
    assert(feof(in));

    SBufferInfo info = {0};
    uint8_t *planes[3] = {0};
    assert((*decoder)->DecodeFrame2(decoder, NULL, 0, planes, &info) == dsErrorFree);
    emit_frame(out, &info, planes);
    int pending = 0;
    assert((*decoder)->GetOption(decoder, DECODER_OPTION_NUM_OF_FRAMES_REMAINING_IN_BUFFER, &pending) == 0);
    while (pending--) {
        memset(&info, 0, sizeof(info));
        assert((*decoder)->FlushFrame(decoder, planes, &info) == dsErrorFree);
        assert(info.iBufferStatus);
        emit_frame(out, &info, planes);
    }
    WelsDestroyDecoder(decoder);
}

static void encode(FILE *in, FILE *out, int main_profile, int width, int height, int fps, int bitrate)
{
    ISVCEncoder *encoder = NULL;
    assert(WelsCreateSVCEncoder(&encoder) == 0);
    SEncParamExt param = {0};
    assert((*encoder)->GetDefaultParams(encoder, &param) == 0);
    param.iPicWidth = width;
    param.iPicHeight = height;
    param.fMaxFrameRate = fps;
    param.iTargetBitrate = bitrate;
    param.iRCMode = RC_BITRATE_MODE;
    param.bEnableFrameSkip = false;
    param.iMultipleThreadIdc = 1;
    param.iEntropyCodingModeFlag = main_profile;
    SSpatialLayerConfig *layer = &param.sSpatialLayers[0];
    layer->iVideoWidth = width;
    layer->iVideoHeight = height;
    layer->fFrameRate = fps;
    layer->iSpatialBitrate = bitrate;
    layer->uiProfileIdc = main_profile ? PRO_MAIN : PRO_BASELINE;
    layer->sSliceArgument.uiSliceMode = SM_SINGLE_SLICE;
    assert((*encoder)->InitializeExt(encoder, &param) == 0);

    size_t luma = (size_t)width * height;
    uint8_t *i420 = malloc(luma * 3 / 2);
    assert(i420);
    SSourcePicture picture = {0};
    picture.iColorFormat = videoFormatI420;
    picture.iPicWidth = width;
    picture.iPicHeight = height;
    picture.iStride[0] = width;
    picture.iStride[1] = picture.iStride[2] = width / 2;
    picture.pData[0] = i420;
    picture.pData[1] = i420 + luma;
    picture.pData[2] = i420 + luma * 5 / 4;
    for (unsigned i = 0; fread(i420, luma * 3 / 2, 1, in) == 1; ++i) {
        picture.uiTimeStamp = (long long)i * 1000 / fps;
        SFrameBSInfo info = {0};
        assert((*encoder)->EncodeFrame(encoder, &picture, &info) == 0);
        assert(info.iFrameSizeInBytes > 0);
        assert(fwrite(&info.iFrameSizeInBytes, 4, 1, out) == 1);
        int bytes = 0;
        for (int l = 0; l < info.iLayerNum; ++l) {
            SLayerBSInfo *part = &info.sLayerInfo[l];
            int size = 0;
            for (int n = 0; n < part->iNalCount; ++n)
                size += part->pNalLengthInByte[n];
            assert(fwrite(part->pBsBuf, 1, size, out) == (size_t)size);
            bytes += size;
        }
        assert(bytes == info.iFrameSizeInBytes);
    }
    assert(feof(in));
    WelsDestroySVCEncoder(encoder);
    free(i420);
}

int main(int argc, char **argv)
{
    // decode IN OUT | baseline|main WIDTH HEIGHT FPS BITRATE IN OUT
    int decoding = strcmp(argv[1], "decode") == 0;
    assert(argc == (decoding ? 4 : 8));
    FILE *in = fopen(argv[argc - 2], "rb");
    FILE *out = fopen(argv[argc - 1], "wb");
    assert(in && out);
    if (decoding)
        decode(in, out);
    else
        encode(in, out, strcmp(argv[1], "main") == 0, atoi(argv[2]), atoi(argv[3]), atoi(argv[4]), atoi(argv[5]));
    assert(fclose(out) == 0);
    assert(fclose(in) == 0);
}
