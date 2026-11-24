#include <climits>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <cstring>

extern "C" {
#include "native.h"
void *__real_calloc(size_t, size_t);
}

#include "codec_api.h"

namespace {
// Matches the resource grant on each session. The accounting includes the
// metadata we pass to malloc as well as the bytes requested by OpenH264.
constexpr size_t heap_cap = 256 * 1024 * 1024;
constexpr int32_t invalid_input = -1;
constexpr int32_t short_output = -2;
constexpr int32_t capacity = -3;
constexpr int32_t finished = -4;

struct Heap {
    size_t used;
    bool refused;
};

thread_local Heap *current_heap;

struct HeapScope {
    explicit HeapScope(Heap &heap) { current_heap = &heap; }
    ~HeapScope() { current_heap = nullptr; }
};

struct alignas(std::max_align_t) Block {
    Heap *owner;
    size_t charged;
};

struct Decoder {
    Heap heap;
    ISVCDecoder *codec;
    bool draining;
};

struct Encoder {
    Heap heap;
    ISVCEncoder *codec;
    uint32_t width, height;
    int32_t packet_size;
    SFrameBSInfo packet;
};

template<class T> T *session()
{
    auto *state = static_cast<T *>(__real_calloc(1, sizeof(T)));
    if (state)
        state->heap.used = sizeof(T);
    return state;
}

int32_t status(int rc, const Heap &heap)
{
    if (heap.refused)
        return capacity;
    return rc == 0 ? 0 : -16 - rc;
}

int64_t frame(const SBufferInfo &info, uint8_t *const planes[3],
              uint8_t *out, uint64_t out_len)
{
    if (!info.iBufferStatus)
        return 0;

    const auto &layout = info.UsrData.sSystemBuffer;
    int width = layout.iWidth;
    int height = layout.iHeight;
    if (width <= 0 || height <= 0 || width % 2 || height % 2 ||
        layout.iStride[0] < width || layout.iStride[1] < width / 2 ||
        !planes[0] || !planes[1] || !planes[2])
        return -16 - dsBitstreamError;

    const uint64_t bytes = uint64_t(width) * height * 3 / 2;
    if (out_len < 8 || bytes > out_len - 8)
        return short_output;

    // The archive target is little endian. The loan may be unaligned.
    std::memcpy(out, &info.uiOutYuvTimeStamp, 8);
    out += 8;
    for (int plane = 0; plane < 3; ++plane) {
        const int shift = plane != 0;
        const int row_bytes = width >> shift;
        const int stride = layout.iStride[shift];
        const uint8_t *src = planes[plane];
        for (int row = 0; row < (height >> shift); ++row) {
            std::memcpy(out, src + size_t(row) * stride, row_bytes);
            out += row_bytes;
        }
    }
    return (int64_t(width) << 32) | height;
}
}

// ld.lld redirects the codec's malloc/calloc/free references to these private
// functions. The ownership header allows a session to move between tasks.
extern "C" void *__real_malloc(size_t);
extern "C" void __real_free(void *);

extern "C" void *__wrap_malloc(size_t bytes)
{
    Heap *heap = current_heap;
    if (!heap)
        return nullptr;
    if (bytes > heap_cap - heap->used || sizeof(Block) > heap_cap - heap->used - bytes) {
        heap->refused = true;
        return nullptr;
    }
    const size_t charged = sizeof(Block) + bytes;
    auto *block = static_cast<Block *>(__real_malloc(charged));
    if (!block) {
        heap->refused = true;
        return nullptr;
    }
    *block = Block{heap, charged};
    heap->used += charged;
    return block + 1;
}

extern "C" void *__wrap_calloc(size_t count, size_t size)
{
    if (size && count > SIZE_MAX / size) {
        if (current_heap)
            current_heap->refused = true;
        return nullptr;
    }
    const size_t bytes = count * size;
    void *p = __wrap_malloc(bytes);
    if (p)
        std::memset(p, 0, bytes);
    return p;
}

extern "C" void __wrap_free(void *memory)
{
    if (memory) {
        auto *block = static_cast<Block *>(memory) - 1;
        block->owner->used -= block->charged;
        __real_free(block);
    }
}

// OpenH264 checks allocation failures. Build its C++ translation units with
// -fcheck-new so a failed allocation skips construction, and localize these
// operators along with the rest of the archive's implementation.
void *operator new(size_t bytes) { return __wrap_malloc(bytes); }
void *operator new[](size_t bytes) { return __wrap_malloc(bytes); }
void operator delete(void *p) noexcept { __wrap_free(p); }
void operator delete[](void *p) noexcept { __wrap_free(p); }

extern "C" void __cxa_pure_virtual() { std::abort(); }

void *hh264_decoder_create()
{
    auto *state = session<Decoder>();
    if (!state)
        return nullptr;

    HeapScope scope(state->heap);
    if (WelsCreateDecoder(&state->codec) == 0) {
        SDecodingParam param{};
        param.uiTargetDqLayer = UCHAR_MAX;
        param.eEcActiveIdc = ERROR_CON_DISABLE;
        param.sVideoProperty.eVideoBsType = VIDEO_BITSTREAM_AVC;
        int threads = 0;
        int trace = WELS_LOG_QUIET;
        state->codec->SetOption(DECODER_OPTION_NUM_OF_THREADS, &threads);
        state->codec->SetOption(DECODER_OPTION_TRACE_LEVEL, &trace);
        if (state->codec->Initialize(&param) == 0 && !state->heap.refused)
            return state;
        WelsDestroyDecoder(state->codec);
    }
    __real_free(state);
    return nullptr;
}

void hh264_decoder_destroy(void *decoder)
{
    auto *state = static_cast<Decoder *>(decoder);
    HeapScope scope(state->heap);
    WelsDestroyDecoder(state->codec);
    __real_free(state);
}

int64_t hh264_decode(void *decoder, const uint8_t *nal_ptr, uint64_t nal_len,
                     uint64_t timestamp, uint8_t *out_ptr, uint64_t out_len)
{
    auto *state = static_cast<Decoder *>(decoder);
    if (state->heap.refused)
        return capacity;
    if (state->draining)
        return finished;
    HeapScope scope(state->heap);
    SBufferInfo info{};
    info.uiInBsTimeStamp = timestamp;
    uint8_t *planes[3]{};
    const auto rc = state->codec->DecodeFrame2(nal_ptr, int(nal_len), planes, &info);
    const int32_t error = status(rc, state->heap);
    return error ? error : frame(info, planes, out_ptr, out_len);
}

int64_t hh264_finish(void *decoder, uint8_t *out_ptr, uint64_t out_len)
{
    auto *state = static_cast<Decoder *>(decoder);
    if (state->heap.refused)
        return capacity;
    HeapScope scope(state->heap);
    SBufferInfo info{};
    uint8_t *planes[3]{};
    if (!state->draining) {
        state->draining = true;
        const auto rc = state->codec->DecodeFrame2(nullptr, 0, planes, &info);
        const int32_t error = status(rc, state->heap);
        if (error || info.iBufferStatus)
            return error ? error : frame(info, planes, out_ptr, out_len);
    }
    const auto rc = state->codec->FlushFrame(planes, &info);
    const int32_t error = status(rc, state->heap);
    return error ? error : frame(info, planes, out_ptr, out_len);
}

void *hh264_encoder_create(uint32_t width, uint32_t height, uint32_t fps,
                           uint32_t bitrate, uint8_t main)
{
    auto *state = session<Encoder>();
    if (!state)
        return nullptr;

    HeapScope scope(state->heap);
    if (WelsCreateSVCEncoder(&state->codec) == 0) {
        SEncParamExt param{};
        state->codec->GetDefaultParams(&param);
        param.iPicWidth = width;
        param.iPicHeight = height;
        param.fMaxFrameRate = float(fps);
        param.iTargetBitrate = bitrate;
        param.iRCMode = RC_BITRATE_MODE;
        param.bEnableFrameSkip = false;
        param.iMultipleThreadIdc = 1;
        param.iEntropyCodingModeFlag = main;
        auto &layer = param.sSpatialLayers[0];
        layer.iVideoWidth = width;
        layer.iVideoHeight = height;
        layer.fFrameRate = float(fps);
        layer.iSpatialBitrate = bitrate;
        layer.uiProfileIdc = main ? PRO_MAIN : PRO_BASELINE;
        layer.sSliceArgument.uiSliceMode = SM_SINGLE_SLICE;

        int trace = WELS_LOG_QUIET;
        state->codec->SetOption(ENCODER_OPTION_TRACE_LEVEL, &trace);
        if (state->codec->InitializeExt(&param) == 0 && !state->heap.refused) {
            state->width = width;
            state->height = height;
            return state;
        }
        WelsDestroySVCEncoder(state->codec);
    }
    __real_free(state);
    return nullptr;
}

void hh264_encoder_destroy(void *encoder)
{
    auto *state = static_cast<Encoder *>(encoder);
    HeapScope scope(state->heap);
    WelsDestroySVCEncoder(state->codec);
    __real_free(state);
}

int32_t hh264_encode(void *encoder, const uint8_t *i420_ptr, uint64_t i420_len, uint64_t timestamp)
{
    auto *state = static_cast<Encoder *>(encoder);
    if (state->heap.refused)
        return capacity;
    const uint64_t luma = uint64_t(state->width) * state->height;
    if (i420_len != luma * 3 / 2 || timestamp > INT64_MAX)
        return invalid_input;

    HeapScope scope(state->heap);
    SSourcePicture picture{};
    picture.iColorFormat = videoFormatI420;
    picture.iPicWidth = state->width;
    picture.iPicHeight = state->height;
    picture.iStride[0] = state->width;
    picture.iStride[1] = picture.iStride[2] = state->width / 2;
    // The pinned encoder copies the source into its own picture before
    // preprocessing. The API predates const on the plane pointers.
    picture.pData[0] = const_cast<uint8_t *>(i420_ptr);
    picture.pData[1] = picture.pData[0] + luma;
    picture.pData[2] = picture.pData[1] + luma / 4;
    picture.uiTimeStamp = timestamp;
    state->packet_size = 0;
    const int rc = state->codec->EncodeFrame(&picture, &state->packet);
    const int32_t error = status(rc, state->heap);
    if (error)
        return error;
    state->packet_size = state->packet.iFrameSizeInBytes;
    return state->packet_size;
}

int32_t hh264_copy_packet(void *encoder, uint8_t *out_ptr, uint64_t out_len)
{
    const auto *state = static_cast<const Encoder *>(encoder);
    if (state->heap.refused)
        return capacity;
    if (uint64_t(state->packet_size) > out_len)
        return short_output;
    // The layers in SFrameBSInfo point into one Annex B output buffer.
    if (state->packet_size)
        std::memcpy(out_ptr, state->packet.sLayerInfo[0].pBsBuf, state->packet_size);
    return state->packet_size;
}
