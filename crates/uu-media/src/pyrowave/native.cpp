// Eclipse's narrow C ABI around the upstream MIT PyroWave C API. No C++
// object, Vulkan dispatch table or borrowed callback crosses the Rust boundary.
#include <vulkan/vulkan.h>
#include <pyrowave.h>
#include <algorithm>
#include <array>
#include <cstdio>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>
#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <d3d11_4.h>
#include <d3d10.h>
#include <dxgi1_2.h>
#include <d3dcompiler.h>
#include <wrl/client.h>
using Microsoft::WRL::ComPtr;
#else
#include <dlfcn.h>
#endif

namespace {
constexpr size_t MaxBitstream = 16 * 1024 * 1024;
void require(bool condition, const char *message) {
    if (!condition) throw std::runtime_error(message);
}
void checked(pyrowave_result result, const char *operation) {
    if (result != PYROWAVE_SUCCESS)
        throw std::runtime_error(std::string(operation) + ": PyroWave result=" + std::to_string(result));
}
void error_text(char *error, size_t capacity, const char *message) {
    if (error && capacity) std::snprintf(error, capacity, "%s", message);
}

#define PW_FUNCTIONS(X) \
    X(pyrowave_get_api_version) X(pyrowave_create_default_device) \
    X(pyrowave_create_device_by_compat) X(pyrowave_device_destroy) \
    X(pyrowave_device_set_queue_type) X(pyrowave_device_confirm_interop_support) \
    X(pyrowave_device_report_performance_stats) \
    X(pyrowave_encoder_create) X(pyrowave_encoder_destroy) \
    X(pyrowave_encoder_encode_cpu) X(pyrowave_encoder_encode_gpu_scaled) \
    X(pyrowave_encoder_compute_num_packets) X(pyrowave_encoder_packetize) \
    X(pyrowave_decoder_create) X(pyrowave_decoder_destroy) X(pyrowave_decoder_clear) \
    X(pyrowave_decoder_device_prefers_fragment_path) X(pyrowave_decoder_push_packet) \
    X(pyrowave_decoder_decode_is_ready) X(pyrowave_decoder_decode_cpu_buffer_synchronous) \
    X(pyrowave_decoder_decode_gpu_buffer) \
    X(pyrowave_image_create) X(pyrowave_image_get_image_view) X(pyrowave_image_destroy) \
    X(pyrowave_sync_object_create) X(pyrowave_sync_object_get_semaphore) \
    X(pyrowave_sync_object_cpu_wait) X(pyrowave_sync_object_destroy)

struct Api {
#ifdef _WIN32
    HMODULE library = nullptr;
#else
    void *library = nullptr;
#endif
#define DECLARE(name) decltype(&::name) name = nullptr;
    PW_FUNCTIONS(DECLARE)
#undef DECLARE
    explicit Api(const char *path) {
        require(path && *path, "missing absolute PyroWave runtime path");
#ifdef _WIN32
        int size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, nullptr, 0);
        require(size > 0, "invalid UTF-8 PyroWave runtime path");
        std::vector<wchar_t> wide(size);
        MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, wide.data(), size);
        library = LoadLibraryExW(wide.data(), nullptr, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
        auto symbol = [&](const char *name) {return reinterpret_cast<void *>(GetProcAddress(library, name));};
#else
        library = dlopen(path, RTLD_NOW | RTLD_LOCAL);
        auto symbol = [&](const char *name) {return dlsym(library, name);};
#endif
        require(library != nullptr, "PyroWave runtime could not be loaded from the installation directory");
        try {
#define LOAD(name) name = reinterpret_cast<decltype(name)>(symbol(#name)); require(name != nullptr, "missing PyroWave entry point: " #name);
            PW_FUNCTIONS(LOAD)
#undef LOAD
            uint32_t major = 0, minor = 0, patch = 0;
            pyrowave_get_api_version(&major, &minor, &patch);
            require(major == 1 && minor == 0 && patch == 0, "unsupported PyroWave C API; expected pinned 1.0.0");
        } catch (...) {close(); throw;}
    }
    void close() {
        if (!library) return;
#ifdef _WIN32
        FreeLibrary(library);
#else
        dlclose(library);
#endif
        library = nullptr;
    }
    ~Api() {close();}
};

#ifdef _WIN32
void hr(HRESULT result, const char *operation) {
    if (FAILED(result)) throw std::runtime_error(std::string(operation) + ": HRESULT=" + std::to_string(uint32_t(result)));
}
struct SharedImage {
    pyrowave_image image = nullptr;
    ComPtr<ID3D11Texture2D> texture;
    ComPtr<ID3D11ShaderResourceView> view;
};
struct Output {
    ComPtr<ID3D11Texture2D> texture;
    ComPtr<ID3D11RenderTargetView> target;
    bool leased = false;
};
struct D3dLock {
    ComPtr<ID3D10Multithread> threading;
    explicit D3dLock(ID3D11DeviceContext *context) {
        hr(context->QueryInterface(IID_PPV_ARGS(&threading)), "lock PyroWave context");
        threading->Enter();
    }
    ~D3dLock() {threading->Leave();}
};
const char ConvertShader[] = R"(
Texture2D<float> luma:register(t0);
Texture2D<float> cb:register(t1);
Texture2D<float> cr:register(t2);
SamplerState bilinear:register(s0);
float4 vertex(uint id:SV_VertexID):SV_Position {
    float2 p=float2((id<<1)&2,id&2); return float4(p.x*2-1,1-p.y*2,0,1);
}
float4 pixel(float4 pos:SV_Position):SV_Target {
    uint w,h; luma.GetDimensions(w,h); float2 uv=pos.xy/float2(w,h);
    float y=luma.Load(int3(pos.xy,0));
    float u=cb.SampleLevel(bilinear,uv,0)-128.0/255.0;
    float v=cr.SampleLevel(bilinear,uv,0)-128.0/255.0;
    return float4(saturate(float3(y+1.5748*v,y-0.187324*u-0.468124*v,y+1.8556*u)),1);
}
)";
#endif

struct Session {
    std::shared_ptr<Api> api;
    pyrowave_device device = nullptr;
    pyrowave_encoder encoder = nullptr;
    pyrowave_decoder decoder = nullptr;
    uint32_t width = 0, height = 0, fps = 0, kbps = 0;
    std::vector<uint8_t> bitstream;
#ifdef _WIN32
    ComPtr<ID3D11Device> d3d;
    ComPtr<ID3D11DeviceContext> context;
    ComPtr<ID3D11DeviceContext4> context4;
    ComPtr<ID3D11Fence> fence;
    pyrowave_sync_object sync = nullptr;
    uint64_t timeline = 0;
    std::array<SharedImage, 3> images;
    uint32_t input_width = 0, input_height = 0;
    ComPtr<ID3D11VertexShader> vertex;
    ComPtr<ID3D11PixelShader> pixel;
    ComPtr<ID3D11SamplerState> sampler;
    std::array<Output, 4> outputs;
#endif
    ~Session() {
        if (encoder) api->pyrowave_encoder_destroy(encoder);
        if (decoder) api->pyrowave_decoder_destroy(decoder);
#ifdef _WIN32
        if (sync) {
            // All submitted D3D reads precede this value. Do not release the
            // imported allocation while the other API still owns it.
            if (timeline) api->pyrowave_sync_object_cpu_wait(sync, timeline, 5'000'000'000ull);
            for (auto &image : images) if (image.image) api->pyrowave_image_destroy(image.image);
            api->pyrowave_sync_object_destroy(sync);
        }
#endif
        if (device) api->pyrowave_device_destroy(device);
    }
    size_t budget() const {
        size_t value = size_t(uint64_t(kbps) * 1000 / 8 / fps) & ~size_t(3);
        require(value >= 1024 && value <= MaxBitstream, "PyroWave frame budget must be 1 KiB to 16 MiB; adjust bitrate or frame rate");
        return value;
    }
    pyrowave_cpu_buffer cpu(void *data) const {
        size_t y = size_t(width) * height;
        auto *bytes = static_cast<uint8_t *>(data);
        pyrowave_cpu_buffer buffer = {};
        buffer.width = int(width); buffer.height = int(height);
        buffer.format = PYROWAVE_CPU_BUFFER_FORMAT_YUV420P;
        buffer.data[0] = bytes; buffer.data[1] = bytes + y; buffer.data[2] = bytes + y + y / 4;
        buffer.row_stride_in_bytes[0] = width;
        buffer.row_stride_in_bytes[1] = buffer.row_stride_in_bytes[2] = width / 2;
        buffer.plane_size_in_bytes[0] = y;
        buffer.plane_size_in_bytes[1] = buffer.plane_size_in_bytes[2] = y / 4;
        return buffer;
    }
    void packetize() {
        // One upstream packet is one complete access unit, including its
        // sequence header. RTP fragments it separately, preserving all bytes.
        size_t count = 0;
        checked(api->pyrowave_encoder_compute_num_packets(encoder, MaxBitstream, &count), "compute packets");
        require(count == 1, "PyroWave access unit exceeds the transport bound");
        if (bitstream.capacity() < budget() + 4096) bitstream.reserve(budget() + 4096);
        bitstream.resize(budget() + 4096);
        pyrowave_packet packet = {};
        checked(api->pyrowave_encoder_packetize(encoder, &packet, MaxBitstream, &count, bitstream.data(), bitstream.size()), "packetize");
        require(count == 1 && packet.offset == 0 && packet.size >= 8 && packet.size <= MaxBitstream && packet.size <= bitstream.size(), "invalid PyroWave access unit");
        bitstream.resize(packet.size);
    }
    void push(const void *data, size_t size) {
        require(decoder && data && size >= 8 && size <= MaxBitstream, "invalid PyroWave decoder input");
        // Every AU is an independent frame. Reset the wrapping three-bit
        // sequence state, so losing eight frames cannot suppress recovery.
        api->pyrowave_decoder_clear(decoder);
        checked(api->pyrowave_decoder_push_packet(decoder, data, size), "push complete frame");
        require(api->pyrowave_decoder_decode_is_ready(decoder, false), "incomplete PyroWave frame");
    }
#ifdef _WIN32
    void init_d3d(ID3D11Device *input) {
        if (input) {d3d = input; d3d->GetImmediateContext(&context);}
        else hr(D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            nullptr, 0, D3D11_SDK_VERSION, &d3d, nullptr, &context), "create PyroWave D3D11 device");
        require(!(d3d->GetCreationFlags() & D3D11_CREATE_DEVICE_SINGLETHREADED), "PyroWave requires a protected D3D11 device");
        ComPtr<ID3D10Multithread> threading;
        hr(context.As(&threading), "D3D11 multithread interface"); threading->SetMultithreadProtected(TRUE);
        ComPtr<IDXGIDevice> dxgi; ComPtr<IDXGIAdapter> adapter;
        hr(d3d.As(&dxgi), "DXGI device"); hr(dxgi->GetAdapter(&adapter), "capture adapter");
        DXGI_ADAPTER_DESC desc = {}; hr(adapter->GetDesc(&desc), "capture adapter LUID");
        static_assert(sizeof(desc.AdapterLuid) == sizeof(pyrowave_luid), "LUID ABI");
        pyrowave_luid luid; std::memcpy(&luid, &desc.AdapterLuid, sizeof(luid));
        auto device_result=api->pyrowave_create_device_by_compat(0, 0, nullptr, nullptr, &luid,
            VK_QUEUE_GLOBAL_PRIORITY_MEDIUM, &device);
        if (device_result!=PYROWAVE_SUCCESS) {
            char details[192];std::snprintf(details,sizeof(details),"create matching Vulkan device (vendor=%04x, LUID=%08x:%08x)",
                unsigned(desc.VendorId),unsigned(desc.AdapterLuid.HighPart),unsigned(desc.AdapterLuid.LowPart));
            checked(device_result,details);
        }
        require(api->pyrowave_device_confirm_interop_support(device), "GPU does not support PyroWave external-memory interop");
        ComPtr<ID3D11Device5> device5;
        hr(d3d.As(&device5), "D3D11 fence device"); hr(context.As(&context4), "D3D11 fence context");
        hr(device5->CreateFence(0, D3D11_FENCE_FLAG_SHARED, IID_PPV_ARGS(&fence)), "create shared D3D11 fence");
        HANDLE handle = nullptr;
        hr(fence->CreateSharedHandle(nullptr, GENERIC_ALL, nullptr, &handle), "export D3D11 fence");
        pyrowave_sync_object_create_info sync_info = {};
        sync_info.device = device; sync_info.external_handle = reinterpret_cast<uintptr_t>(handle);
        sync_info.handle_type = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT;
        sync_info.semaphore_type = VK_SEMAPHORE_TYPE_TIMELINE;
        auto result = api->pyrowave_sync_object_create(&sync_info, &sync);
        if (result != PYROWAVE_SUCCESS) CloseHandle(handle); // consumed only on success
        checked(result, "import D3D11 fence");
    }
    void shared_image(SharedImage &output, uint32_t w, uint32_t h, bool rgb) {
        D3D11_TEXTURE2D_DESC desc = {};
        desc.Width = w; desc.Height = h; desc.ArraySize = desc.MipLevels = desc.SampleDesc.Count = 1;
        desc.Format = rgb ? DXGI_FORMAT_B8G8R8A8_UNORM : DXGI_FORMAT_R8_UNORM;
        desc.Usage = D3D11_USAGE_DEFAULT;
        desc.BindFlags = D3D11_BIND_SHADER_RESOURCE | (rgb ? D3D11_BIND_RENDER_TARGET : D3D11_BIND_UNORDERED_ACCESS);
        desc.MiscFlags = D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE;
        hr(d3d->CreateTexture2D(&desc, nullptr, &output.texture), "create shared PyroWave texture");
        ComPtr<IDXGIResource1> resource; hr(output.texture.As(&resource), "shared texture resource");
        HANDLE handle = nullptr;
        hr(resource->CreateSharedHandle(nullptr, DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE, nullptr, &handle), "export D3D11 texture");
        VkImageCreateInfo vk = {}; vk.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
        vk.imageType = VK_IMAGE_TYPE_2D; vk.format = rgb ? VK_FORMAT_B8G8R8A8_UNORM : VK_FORMAT_R8_UNORM;
        vk.extent = {w, h, 1}; vk.mipLevels = vk.arrayLayers = 1;
        vk.samples = VK_SAMPLE_COUNT_1_BIT; vk.tiling = VK_IMAGE_TILING_OPTIMAL;
        vk.usage = VK_IMAGE_USAGE_SAMPLED_BIT | (rgb ? VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT : VK_IMAGE_USAGE_STORAGE_BIT);
        vk.sharingMode = VK_SHARING_MODE_EXCLUSIVE; vk.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
        pyrowave_image_create_info info = {};
        info.device = device; info.external_handle = reinterpret_cast<uintptr_t>(handle);
        info.handle_type = VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT; info.image_create_info = &vk;
        auto result = api->pyrowave_image_create(&info, &output.image);
        if (result != PYROWAVE_SUCCESS) CloseHandle(handle);
        checked(result, "import D3D11 texture");
        if (!rgb) hr(d3d->CreateShaderResourceView(output.texture.Get(), nullptr, &output.view), "plane shader view");
    }
    void encode_gpu(ID3D11Texture2D *source) {
        require(encoder && d3d && source, "PyroWave encoder has no GPU input device");
        ComPtr<ID3D11Device> owner; source->GetDevice(&owner);
        require(owner.Get() == d3d.Get(), "PyroWave capture texture belongs to a different D3D11 device");
        D3D11_TEXTURE2D_DESC desc = {}; source->GetDesc(&desc);
        require(desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM && desc.MipLevels == 1 && desc.ArraySize == 1
            && desc.SampleDesc.Count == 1 && desc.Width >= width && desc.Height >= height,
            "PyroWave GPU input requires a complete SDR BGRA surface");
        if (!images[0].image || input_width != desc.Width || input_height != desc.Height) {
            if (images[0].image) {
                checked(api->pyrowave_sync_object_cpu_wait(sync, timeline, 5'000'000'000ull), "wait before input resize");
                api->pyrowave_image_destroy(images[0].image); images[0] = {};
            }
            shared_image(images[0], desc.Width, desc.Height, true);
            input_width = desc.Width; input_height = desc.Height;
        }
        context->CopyResource(images[0].texture.Get(), source);
        uint64_t acquire_value = ++timeline;
        hr(context4->Signal(fence.Get(), acquire_value), "signal captured texture"); context->Flush();
        pyrowave_gpu_external_reference ref = {images[0].image, VK_QUEUE_FAMILY_EXTERNAL};
        auto semaphore = api->pyrowave_sync_object_get_semaphore(sync);
        pyrowave_gpu_sync_operation acquire = {&ref, 1, {semaphore, acquire_value}};
        pyrowave_gpu_sync_operation release = {&ref, 1, {semaphore, ++timeline}};
        pyrowave_scaled_encode_info scaled = {};
        checked(api->pyrowave_image_get_image_view(images[0].image, VK_IMAGE_ASPECT_COLOR_BIT,
            VK_IMAGE_USAGE_SAMPLED_BIT, &scaled.view), "capture image view");
        scaled.input_color_space = scaled.output_color_space = VK_COLOR_SPACE_SRGB_NONLINEAR_KHR;
        scaled.intermediate_plane_format = VK_FORMAT_R8_UNORM;
        scaled.ycbcr_chroma_midpoint = 128.f / 255.f;
        scaled.ycbcr_range = VK_SAMPLER_YCBCR_RANGE_ITU_FULL; scaled.ycbcr_range_bit_depth = 8;
        pyrowave_rate_control rate = {budget()};
        checked(api->pyrowave_encoder_encode_gpu_scaled(encoder, &acquire, &release, &scaled, &rate), "encode shared GPU texture");
        // Packetization waits for bitstream readback and completes the Vulkan
        // input read. The caller can then return its capture lease safely.
        packetize();
        hr(context4->Wait(fence.Get(), timeline), "return input ownership to D3D11");
    }
    void init_decode_gpu() {
        for (size_t i = 0; i < images.size(); i++) shared_image(images[i], i ? width / 2 : width, i ? height / 2 : height, false);
        ComPtr<ID3DBlob> vs, ps, errors;
        hr(D3DCompile(ConvertShader, sizeof(ConvertShader), nullptr, nullptr, nullptr, "vertex", "vs_5_0", D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &vs, &errors), "compile PyroWave vertex shader");
        hr(D3DCompile(ConvertShader, sizeof(ConvertShader), nullptr, nullptr, nullptr, "pixel", "ps_5_0", D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &ps, &errors), "compile PyroWave colour shader");
        hr(d3d->CreateVertexShader(vs->GetBufferPointer(), vs->GetBufferSize(), nullptr, &vertex), "create PyroWave vertex shader");
        hr(d3d->CreatePixelShader(ps->GetBufferPointer(), ps->GetBufferSize(), nullptr, &pixel), "create PyroWave colour shader");
        D3D11_SAMPLER_DESC desc = {}; desc.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR;
        desc.AddressU = desc.AddressV = desc.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP; desc.MaxLOD = D3D11_FLOAT32_MAX;
        hr(d3d->CreateSamplerState(&desc, &sampler), "create PyroWave sampler");
    }
    int decode_gpu() {
        D3dLock lock(context.Get());
        size_t index = 0;
        for (; index < outputs.size() && outputs[index].leased; index++) {}
        if (index == outputs.size()) return -1; // caller drops the independent frame
        auto &output = outputs[index];
        if (!output.texture) {
            D3D11_TEXTURE2D_DESC desc = {}; desc.Width = width; desc.Height = height;
            desc.MipLevels = desc.ArraySize = desc.SampleDesc.Count = 1; desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
            desc.Usage = D3D11_USAGE_DEFAULT; desc.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
            hr(d3d->CreateTexture2D(&desc, nullptr, &output.texture), "allocate leased PyroWave RGB output");
            hr(d3d->CreateRenderTargetView(output.texture.Get(), nullptr, &output.target), "PyroWave RGB target");
        }
        std::array<pyrowave_gpu_external_reference, 3> refs;
        pyrowave_gpu_buffers buffers = {};
        for (size_t i = 0; i < images.size(); i++) {
            refs[i] = {images[i].image, VK_QUEUE_FAMILY_EXTERNAL};
            checked(api->pyrowave_image_get_image_view(images[i].image, VK_IMAGE_ASPECT_COLOR_BIT,
                VK_IMAGE_USAGE_STORAGE_BIT, &buffers.planes[i]), "decoder plane view");
        }
        auto semaphore = api->pyrowave_sync_object_get_semaphore(sync);
        pyrowave_gpu_sync_operation acquire = {refs.data(), refs.size(), {semaphore, timeline}};
        pyrowave_gpu_sync_operation release = {refs.data(), refs.size(), {semaphore, ++timeline}};
        checked(api->pyrowave_decoder_decode_gpu_buffer(decoder, &acquire, &release, &buffers), "decode to shared GPU planes");
        hr(context4->Wait(fence.Get(), timeline), "wait for decoded GPU planes");
        // All texture writes and renderer reads use the same protected context.
        // Vulkan waits for this context's release before reusing the planes.
        ID3D11RenderTargetView *target = output.target.Get();
        context->OMSetRenderTargets(1, &target, nullptr);
        context->OMSetBlendState(nullptr, nullptr, 0xffffffffu); context->OMSetDepthStencilState(nullptr, 0);
        context->RSSetState(nullptr); D3D11_VIEWPORT viewport = {0, 0, float(width), float(height), 0, 1};
        context->RSSetViewports(1, &viewport); context->IASetInputLayout(nullptr);
        context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        context->VSSetShader(vertex.Get(), nullptr, 0); context->PSSetShader(pixel.Get(), nullptr, 0);
        context->GSSetShader(nullptr, nullptr, 0); context->HSSetShader(nullptr, nullptr, 0); context->DSSetShader(nullptr, nullptr, 0);
        ID3D11ShaderResourceView *views[] = {images[0].view.Get(), images[1].view.Get(), images[2].view.Get()};
        context->PSSetShaderResources(0, 3, views); auto *sample = sampler.Get(); context->PSSetSamplers(0, 1, &sample);
        context->Draw(3, 0);
        ID3D11ShaderResourceView *empty[] = {nullptr, nullptr, nullptr}; context->PSSetShaderResources(0, 3, empty);
        context->OMSetRenderTargets(0, nullptr, nullptr);
        hr(context4->Signal(fence.Get(), ++timeline), "release decoded GPU planes"); context->Flush();
        output.leased = true;
        return int(index);
    }
#endif
};

template<typename F> int invoke(char *error, size_t capacity, F &&function) noexcept {
    try {function(); return 0;}
    catch (const std::exception &e) {error_text(error, capacity, e.what()); return -1;}
    catch (...) {error_text(error, capacity, "unknown PyroWave adapter failure"); return -1;}
}
}

extern "C" {
struct EclipsePyroConfig {uint32_t width, height, fps, kbps, encode, gpu;};
void *eclipse_pw_create(const char *library, const EclipsePyroConfig *info, void *input_device,
                       char *error, size_t capacity) noexcept {
    void *result = nullptr;
    invoke(error, capacity, [&] {
        require(info && info->width >= 2 && info->height >= 2 && info->width <= 8192 && info->height <= 8192
            && !(info->width & 1) && !(info->height & 1) && info->fps >= 5 && info->fps <= 240, "invalid PyroWave geometry or frame rate");
        auto session = std::make_unique<Session>(); session->api = std::make_shared<Api>(library);
        auto &s = *session; s.width = info->width; s.height = info->height; s.fps = info->fps; s.kbps = info->kbps;
#ifdef _WIN32
        if (info->gpu) s.init_d3d(static_cast<ID3D11Device *>(input_device));
        else
#else
        (void)input_device;
        require(!info->gpu, "PyroWave external GPU buffers are unavailable on this platform");
#endif
        checked(s.api->pyrowave_create_default_device(&s.device), "create Vulkan device");
        checked(s.api->pyrowave_device_set_queue_type(s.device, info->encode ? VK_QUEUE_COMPUTE_BIT : VK_QUEUE_GRAPHICS_BIT), "select Vulkan queue");
        if (info->encode) {
            s.budget();
            pyrowave_encoder_create_info create = {s.device, int(s.width), int(s.height), PYROWAVE_CHROMA_SUBSAMPLING_420};
            checked(s.api->pyrowave_encoder_create(&create, &s.encoder), "create encoder");
        } else {
            pyrowave_decoder_create_info create = {s.device, int(s.width), int(s.height), PYROWAVE_CHROMA_SUBSAMPLING_420,
                s.api->pyrowave_decoder_device_prefers_fragment_path(s.device)};
            checked(s.api->pyrowave_decoder_create(&create, &s.decoder), "create decoder");
#ifdef _WIN32
            if (info->gpu) s.init_decode_gpu();
#endif
        }
        result = session.release();
    });
    return result;
}
void eclipse_pw_destroy(void *opaque) noexcept {delete static_cast<Session *>(opaque);}
int eclipse_pw_set_rate(void *opaque, uint32_t kbps, uint32_t fps, char *error, size_t capacity) noexcept {
    return invoke(error, capacity, [&] {
        auto &s = *static_cast<Session *>(opaque); require(fps >= 5 && fps <= 240, "invalid PyroWave frame rate");
        auto previous_kbps = s.kbps, previous_fps = s.fps; s.kbps = kbps; s.fps = fps;
        try {s.budget();} catch (...) {s.kbps = previous_kbps; s.fps = previous_fps; throw;}
    });
}
int eclipse_pw_encode(void *opaque, const void *pixels, size_t size, void *texture,
                      const void **data, size_t *length, char *error, size_t capacity) noexcept {
    return invoke(error, capacity, [&] {
        auto &s = *static_cast<Session *>(opaque); require(s.encoder && data && length, "invalid PyroWave encoder handle");
#ifdef _WIN32
        if (texture) s.encode_gpu(static_cast<ID3D11Texture2D *>(texture));
        else
#else
        require(!texture, "PyroWave GPU input is unavailable on this platform");
#endif
        {
            require(pixels && size == size_t(s.width) * s.height * 3 / 2, "invalid packed PyroWave I420 input");
            auto buffer = s.cpu(const_cast<void *>(pixels)); pyrowave_rate_control rate = {s.budget()};
            checked(s.api->pyrowave_encoder_encode_cpu(s.encoder, &buffer, &rate), "encode CPU-uploaded planes"); s.packetize();
        }
        *data = s.bitstream.data(); *length = s.bitstream.size();
    });
}
int eclipse_pw_decode(void *opaque, const void *data, size_t length, void *pixels, size_t size,
                      int *slot, void **texture, char *error, size_t capacity) noexcept {
    return invoke(error, capacity, [&] {
        auto &s = *static_cast<Session *>(opaque); s.push(data, length);
#ifdef _WIN32
        if (s.d3d) {
            require(slot && texture, "missing PyroWave output lease");
            *slot = s.decode_gpu(); *texture = *slot < 0 ? nullptr : s.outputs[size_t(*slot)].texture.Get();
            return;
        }
#else
        (void)slot; (void)texture;
#endif
        require(pixels && size == size_t(s.width) * s.height * 3 / 2, "invalid packed PyroWave decode output");
        auto buffer = s.cpu(pixels);
        checked(s.api->pyrowave_decoder_decode_cpu_buffer_synchronous(s.decoder, &buffer), "decode CPU-readback planes");
    });
}
void eclipse_pw_release(void *opaque, int slot) noexcept {
#ifdef _WIN32
    auto &s = *static_cast<Session *>(opaque);
    if (slot >= 0 && size_t(slot) < s.outputs.size()) s.outputs[size_t(slot)].leased = false;
#else
    (void)opaque; (void)slot;
#endif
}
int eclipse_pw_stats(void *opaque, char *output, size_t capacity) noexcept {
    return invoke(output, capacity, [&] {
        auto &s = *static_cast<Session *>(opaque); std::string text;
        s.api->pyrowave_device_report_performance_stats(s.device, [](void *user, const char *line) {
            auto &value = *static_cast<std::string *>(user);
            if (value.size() < 8192) {value += line; value += '\n';}
        }, &text, false);
        error_text(output, capacity, text.c_str());
    });
}
}
