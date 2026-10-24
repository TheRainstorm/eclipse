// Our narrow AMF adapter. SDK headers retain AMD's MIT notices.
#include <windows.h>
#include <d3d11.h>
#include <d3d10.h>
#include <dxgi1_2.h>
#include <cstdio>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>
#include "core/Factory.h"
#include "components/VideoEncoderVCE.h"
#include "components/VideoEncoderHEVC.h"
#include "components/VideoEncoderAV1.h"
#include "components/ColorSpace.h"

static void checked(AMF_RESULT result,const char* operation) {
    if(result!=AMF_OK) throw std::runtime_error(std::string(operation)+": AMF="+std::to_string(result));
}
static void hr(HRESULT result,const char* operation) {
    if(FAILED(result)) throw std::runtime_error(std::string(operation)+": HRESULT="+std::to_string((unsigned long)result));
}
struct Session {
    HMODULE library=nullptr;
    amf::AMFContextPtr context;
    amf::AMFComponentPtr encoder;
    amf::AMFBufferPtr output;
    ID3D11Device* device=nullptr;
    int codec=0;
    unsigned width=0,height=0,fps=0;
    ~Session() {
        output=nullptr;
        if(encoder) encoder->Terminate();
        encoder=nullptr;
        if(context) context->Terminate();
        context=nullptr;
        if(device) device->Release();
        if(library) FreeLibrary(library);
    }
    const wchar_t* prop(const wchar_t* h264,const wchar_t* hevc,const wchar_t* av1)const {
        return codec==0?h264:codec==1?hevc:av1;
    }
    template<class T> void set(const wchar_t* name,T value) {
        AMF_RESULT result=encoder->SetProperty(name,value);
        if(result!=AMF_OK) {
            std::wstring wide(name);std::string label(wide.begin(),wide.end());
            throw std::runtime_error("AMF property "+label+": "+std::to_string(result));
        }
    }
    void bitrate(unsigned kbps) {
        set(prop(AMF_VIDEO_ENCODER_TARGET_BITRATE,AMF_VIDEO_ENCODER_HEVC_TARGET_BITRATE,AMF_VIDEO_ENCODER_AV1_TARGET_BITRATE),amf_int64(kbps)*1000);
    }
};
static ID3D11Device* amd_device(bool default_adapter) {
    if(default_adapter) {
        ID3D11Device* device=nullptr;D3D_FEATURE_LEVEL level=D3D_FEATURE_LEVEL_11_0;
        hr(D3D11CreateDevice(nullptr,D3D_DRIVER_TYPE_HARDWARE,nullptr,D3D11_CREATE_DEVICE_VIDEO_SUPPORT|D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            &level,1,D3D11_SDK_VERSION,&device,nullptr,nullptr),"create default video device");
        return device;
    }
    IDXGIFactory1* factory=nullptr;
    hr(CreateDXGIFactory1(__uuidof(IDXGIFactory1),(void**)&factory),"create DXGI factory");
    ID3D11Device* device=nullptr;
    for(unsigned i=0;i<128;i++) {
        IDXGIAdapter1* adapter=nullptr;
        if(factory->EnumAdapters1(i,&adapter)==DXGI_ERROR_NOT_FOUND)break;
        if(!adapter)continue;
        DXGI_ADAPTER_DESC1 desc{};adapter->GetDesc1(&desc);
        if(desc.VendorId==0x1002 && !(desc.Flags&DXGI_ADAPTER_FLAG_SOFTWARE)) {
            D3D_FEATURE_LEVEL level=D3D_FEATURE_LEVEL_11_0;
            HRESULT result=D3D11CreateDevice(adapter,D3D_DRIVER_TYPE_UNKNOWN,nullptr,
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT|D3D11_CREATE_DEVICE_BGRA_SUPPORT,&level,1,D3D11_SDK_VERSION,&device,nullptr,nullptr);
            adapter->Release();
            if(SUCCEEDED(result))break;
        } else adapter->Release();
    }
    factory->Release();
    if(!device)throw std::runtime_error("no AMD D3D11 video device");
    return device;
}
static void check_amd(ID3D11Device* device) {
    IDXGIDevice* dxgi=nullptr;hr(device->QueryInterface(__uuidof(IDXGIDevice),(void**)&dxgi),"query DXGI device");
    IDXGIAdapter* adapter=nullptr;HRESULT result=dxgi->GetAdapter(&adapter);dxgi->Release();hr(result,"get adapter");
    DXGI_ADAPTER_DESC desc{};result=adapter->GetDesc(&desc);adapter->Release();hr(result,"get adapter description");
    if(desc.VendorId!=0x1002)throw std::runtime_error("AMF input requires an AMD device");
    ID3D11DeviceContext* context=nullptr;device->GetImmediateContext(&context);
    ID3D10Multithread* multithread=nullptr;
    result=context->QueryInterface(__uuidof(ID3D10Multithread),(void**)&multithread);context->Release();hr(result,"context threading");
    multithread->SetMultithreadProtected(TRUE);multithread->Release();
}
extern "C" void* eclipse_amf_open(int codec,unsigned width,unsigned height,unsigned fps,unsigned kbps,
    unsigned peak,int vbr,int quality,int default_adapter,void* native_device,char* error,size_t capacity) {
    try {
        if(codec<0||codec>2||quality<0||quality>2||width<2||height<2||!fps||!kbps)throw std::runtime_error("invalid AMF settings");
        auto s=std::make_unique<Session>();s->codec=codec;s->width=width;s->height=height;s->fps=fps;
        s->library=LoadLibraryExW(L"amfrt64.dll",nullptr,LOAD_LIBRARY_SEARCH_SYSTEM32);
        if(!s->library)throw std::runtime_error("AMD AMF runtime is not installed in the system directory");
        auto init=(AMFInit_Fn)GetProcAddress(s->library,AMF_INIT_FUNCTION_NAME);
        auto query=(AMFQueryVersion_Fn)GetProcAddress(s->library,AMF_QUERY_VERSION_FUNCTION_NAME);
        if(!init||!query)throw std::runtime_error("invalid AMF runtime exports");
        amf_uint64 version=0;checked(query(&version),"AMFQueryVersion");
        amf::AMFFactory* factory=nullptr;checked(init(version<AMF_FULL_VERSION?version:AMF_FULL_VERSION,&factory),"AMFInit");
        if(native_device){s->device=(ID3D11Device*)native_device;s->device->AddRef();}else s->device=amd_device(default_adapter!=0);
        check_amd(s->device);
        checked(factory->CreateContext(&s->context),"create context");
        checked(s->context->InitDX11(s->device,amf::AMF_DX11_1),"initialize DX11 context");
        checked(factory->CreateComponent(s->context,codec==0?AMFVideoEncoderVCE_AVC:codec==1?AMFVideoEncoder_HEVC:AMFVideoEncoder_AV1,&s->encoder),"create encoder component");
        s->set(s->prop(AMF_VIDEO_ENCODER_USAGE,AMF_VIDEO_ENCODER_HEVC_USAGE,AMF_VIDEO_ENCODER_AV1_USAGE),
            amf_int64(codec==0?AMF_VIDEO_ENCODER_USAGE_ULTRA_LOW_LATENCY:codec==1?AMF_VIDEO_ENCODER_HEVC_USAGE_ULTRA_LOW_LATENCY:AMF_VIDEO_ENCODER_AV1_USAGE_ULTRA_LOW_LATENCY));
        // AMF's AV1 default accepts only 64x16-aligned dimensions. Let the
        // SDK handle padding without changing the requested visible geometry.
        if(codec==2 && (width%64!=0 || height%16!=0)) {
            s->set(AMF_VIDEO_ENCODER_AV1_ALIGNMENT_MODE,amf_int64(AMF_VIDEO_ENCODER_AV1_ALIGNMENT_MODE_NO_RESTRICTIONS));
        }
        const amf_int64 presets[3][3]={
            {AMF_VIDEO_ENCODER_QUALITY_PRESET_SPEED,AMF_VIDEO_ENCODER_QUALITY_PRESET_BALANCED,AMF_VIDEO_ENCODER_QUALITY_PRESET_QUALITY},
            {AMF_VIDEO_ENCODER_HEVC_QUALITY_PRESET_SPEED,AMF_VIDEO_ENCODER_HEVC_QUALITY_PRESET_BALANCED,AMF_VIDEO_ENCODER_HEVC_QUALITY_PRESET_QUALITY},
            {AMF_VIDEO_ENCODER_AV1_QUALITY_PRESET_SPEED,AMF_VIDEO_ENCODER_AV1_QUALITY_PRESET_BALANCED,AMF_VIDEO_ENCODER_AV1_QUALITY_PRESET_QUALITY}};
        const auto quality_name=s->prop(AMF_VIDEO_ENCODER_QUALITY_PRESET,AMF_VIDEO_ENCODER_HEVC_QUALITY_PRESET,AMF_VIDEO_ENCODER_AV1_QUALITY_PRESET);
        s->set(s->prop(AMF_VIDEO_ENCODER_QUALITY_PRESET,AMF_VIDEO_ENCODER_HEVC_QUALITY_PRESET,AMF_VIDEO_ENCODER_AV1_QUALITY_PRESET),
            presets[codec][quality]);
        s->set(s->prop(AMF_VIDEO_ENCODER_FRAMESIZE,AMF_VIDEO_ENCODER_HEVC_FRAMESIZE,AMF_VIDEO_ENCODER_AV1_FRAMESIZE),AMFConstructSize(width,height));
        s->set(s->prop(AMF_VIDEO_ENCODER_FRAMERATE,AMF_VIDEO_ENCODER_HEVC_FRAMERATE,AMF_VIDEO_ENCODER_AV1_FRAMERATE),AMFConstructRate(fps,1));
        s->set(s->prop(AMF_VIDEO_ENCODER_RATE_CONTROL_METHOD,AMF_VIDEO_ENCODER_HEVC_RATE_CONTROL_METHOD,AMF_VIDEO_ENCODER_AV1_RATE_CONTROL_METHOD),
            amf_int64(codec==0?(vbr?AMF_VIDEO_ENCODER_RATE_CONTROL_METHOD_PEAK_CONSTRAINED_VBR:AMF_VIDEO_ENCODER_RATE_CONTROL_METHOD_CBR):
            codec==1?(vbr?AMF_VIDEO_ENCODER_HEVC_RATE_CONTROL_METHOD_PEAK_CONSTRAINED_VBR:AMF_VIDEO_ENCODER_HEVC_RATE_CONTROL_METHOD_CBR):
            (vbr?AMF_VIDEO_ENCODER_AV1_RATE_CONTROL_METHOD_PEAK_CONSTRAINED_VBR:AMF_VIDEO_ENCODER_AV1_RATE_CONTROL_METHOD_CBR)));
        s->bitrate(kbps);
        s->set(s->prop(AMF_VIDEO_ENCODER_PEAK_BITRATE,AMF_VIDEO_ENCODER_HEVC_PEAK_BITRATE,AMF_VIDEO_ENCODER_AV1_PEAK_BITRATE),amf_int64(peak)*1000);
        s->set(s->prop(AMF_VIDEO_ENCODER_INPUT_COLOR_PROFILE,AMF_VIDEO_ENCODER_HEVC_INPUT_COLOR_PROFILE,AMF_VIDEO_ENCODER_AV1_INPUT_COLOR_PROFILE),amf_int64(AMF_VIDEO_CONVERTER_COLOR_PROFILE_601));
        s->set(s->prop(AMF_VIDEO_ENCODER_OUTPUT_COLOR_PROFILE,AMF_VIDEO_ENCODER_HEVC_OUTPUT_COLOR_PROFILE,AMF_VIDEO_ENCODER_AV1_OUTPUT_COLOR_PROFILE),amf_int64(AMF_VIDEO_CONVERTER_COLOR_PROFILE_601));
        s->set(s->prop(AMF_VIDEO_ENCODER_OUTPUT_COLOR_PRIMARIES,AMF_VIDEO_ENCODER_HEVC_OUTPUT_COLOR_PRIMARIES,AMF_VIDEO_ENCODER_AV1_OUTPUT_COLOR_PRIMARIES),amf_int64(AMF_COLOR_PRIMARIES_SMPTE170M));
        s->set(s->prop(AMF_VIDEO_ENCODER_OUTPUT_TRANSFER_CHARACTERISTIC,AMF_VIDEO_ENCODER_HEVC_OUTPUT_TRANSFER_CHARACTERISTIC,AMF_VIDEO_ENCODER_AV1_OUTPUT_TRANSFER_CHARACTERISTIC),amf_int64(AMF_COLOR_TRANSFER_CHARACTERISTIC_BT709));
        if(codec==0) {
            // Windows' H.264 DXVA path requires Main-compatible Baseline.
            // Plain AMF Baseline omits constraint_set1_flag and the system MFT
            // can decode on the CPU while still returning a GPU texture.
            s->set(AMF_VIDEO_ENCODER_PROFILE,amf_int64(AMF_VIDEO_ENCODER_PROFILE_CONSTRAINED_BASELINE));
            s->set(AMF_VIDEO_ENCODER_PROFILE_LEVEL,amf_int64(52));
            s->set(AMF_VIDEO_ENCODER_B_PIC_PATTERN,amf_int64(0));
        }
        // Zero is the documented AMF infinite-GOP value for these formats.
        // Recovery and reference-invalidating reconfiguration still request
        // independently decodable pictures through the shared encoder worker.
        const auto gop_name=s->prop(AMF_VIDEO_ENCODER_IDR_PERIOD,AMF_VIDEO_ENCODER_HEVC_GOP_SIZE,AMF_VIDEO_ENCODER_AV1_GOP_SIZE);
        s->set(gop_name,amf_int64(0));
        // A native timed output wait avoids a fixed application sleep per frame.
        s->encoder->SetProperty(s->prop(AMF_VIDEO_ENCODER_QUERY_TIMEOUT,AMF_VIDEO_ENCODER_HEVC_QUERY_TIMEOUT,AMF_VIDEO_ENCODER_AV1_QUERY_TIMEOUT),amf_int64(1));
        checked(s->encoder->Init(amf::AMF_SURFACE_NV12,width,height),"initialize encoder");
        amf_int64 actual_gop=-1;
        checked(s->encoder->GetProperty(gop_name,&actual_gop),"read GOP policy");
        if(actual_gop!=0)throw std::runtime_error("AMF changed the requested on-demand keyframe policy");
        amf_int64 actual_quality=-1;
        checked(s->encoder->GetProperty(quality_name,&actual_quality),"read quality preset");
        if(actual_quality!=presets[codec][quality])throw std::runtime_error("AMF changed the requested quality preset");
        AMFSize actual_size{};AMFRate actual_rate{};
        checked(s->encoder->GetProperty(s->prop(AMF_VIDEO_ENCODER_FRAMESIZE,AMF_VIDEO_ENCODER_HEVC_FRAMESIZE,AMF_VIDEO_ENCODER_AV1_FRAMESIZE),&actual_size),"read frame size");
        checked(s->encoder->GetProperty(s->prop(AMF_VIDEO_ENCODER_FRAMERATE,AMF_VIDEO_ENCODER_HEVC_FRAMERATE,AMF_VIDEO_ENCODER_AV1_FRAMERATE),&actual_rate),"read frame rate");
        if(actual_size.width!=(int)width||actual_size.height!=(int)height||actual_rate.num!=fps||actual_rate.den!=1)throw std::runtime_error("AMF changed the requested geometry or frame rate");
        return s.release();
    } catch(const std::exception& e) {if(capacity)std::snprintf(error,capacity,"%s",e.what());return nullptr;}
}
extern "C" void eclipse_amf_close(void* session) {delete (Session*)session;}
extern "C" void* eclipse_amf_device(void* session) {return ((Session*)session)->device;}
extern "C" int eclipse_amf_bitrate(void* session,unsigned kbps,char* error,size_t capacity) {
    try {((Session*)session)->bitrate(kbps);return 0;}
    catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}
}
extern "C" int eclipse_amf_encode(void* session,void* native_texture,const unsigned char* cpu_nv12,
    size_t cpu_length,long long pts,int keyframe,const unsigned char** bytes,size_t* length,long long* timestamp,char* error,size_t capacity) {
    try {
        auto s=(Session*)session;s->output=nullptr;
        amf::AMFSurfacePtr surface;
        if(!native_texture) {
            if(!cpu_nv12||cpu_length!=size_t(s->width)*s->height*3/2)throw std::runtime_error("invalid CPU NV12 input");
            // Let AMF own CPU allocation, plane alignment and the upload. A
            // fresh external NV12 texture for every CPU picture crashes some
            // drivers after sustained encoding; it is not a capture surface.
            checked(s->context->AllocSurface(amf::AMF_MEMORY_HOST,amf::AMF_SURFACE_NV12,s->width,s->height,&surface),"allocate CPU NV12 surface");
            if(surface->GetPlanesCount()!=2)throw std::runtime_error("invalid AMF NV12 plane count");
            for(unsigned index=0;index<2;index++) {
                auto plane=surface->GetPlaneAt(index);
                const unsigned rows=index?s->height/2:s->height;
                if(!plane||!plane->GetNative()||plane->GetHPitch()<(int)s->width||plane->GetHeight()<(int)rows)
                    throw std::runtime_error("invalid AMF CPU NV12 plane layout");
                auto dst=(unsigned char*)plane->GetNative();
                const auto src=cpu_nv12+(index?size_t(s->width)*s->height:0);
                for(unsigned row=0;row<rows;row++)std::memcpy(dst+size_t(row)*plane->GetHPitch(),src+size_t(row)*s->width,s->width);
            }
        } else {
            // The Rust caller keeps its immutable, same-device GPU texture
            // leased through this call. No CPU mapping or upload here.
            auto texture=(ID3D11Texture2D*)native_texture;
            static const GUID array_index_id={0x28115527,0xe7c3,0x4b66,{0x99,0xd3,0x4f,0x2a,0xe6,0xb4,0x7f,0xaf}};
            unsigned array_index=0;
            hr(texture->SetPrivateData(array_index_id,sizeof(array_index),&array_index),"set DX11 texture array index");
            checked(s->context->CreateSurfaceFromDX11Native(texture,&surface,nullptr),"wrap DX11 surface");
        }
        surface->SetCrop(0,0,s->width,s->height);
        surface->SetPts(pts);surface->SetDuration(10000000/s->fps);
        if(keyframe) {
            if(s->codec==0) {
                checked(surface->SetProperty(AMF_VIDEO_ENCODER_FORCE_PICTURE_TYPE,amf_int64(AMF_VIDEO_ENCODER_PICTURE_TYPE_IDR)),"force H264 IDR");
                checked(surface->SetProperty(AMF_VIDEO_ENCODER_INSERT_SPS,true),"insert SPS");
                checked(surface->SetProperty(AMF_VIDEO_ENCODER_INSERT_PPS,true),"insert PPS");
            } else if(s->codec==1) {
                checked(surface->SetProperty(AMF_VIDEO_ENCODER_HEVC_FORCE_PICTURE_TYPE,amf_int64(AMF_VIDEO_ENCODER_HEVC_PICTURE_TYPE_IDR)),"force HEVC IDR");
                checked(surface->SetProperty(AMF_VIDEO_ENCODER_HEVC_INSERT_HEADER,true),"insert HEVC headers");
            } else {
                checked(surface->SetProperty(AMF_VIDEO_ENCODER_AV1_FORCE_FRAME_TYPE,amf_int64(AMF_VIDEO_ENCODER_AV1_FORCE_FRAME_TYPE_KEY)),"force AV1 keyframe");
                checked(surface->SetProperty(AMF_VIDEO_ENCODER_AV1_FORCE_INSERT_SEQUENCE_HEADER,true),"insert AV1 sequence");
            }
        }
        checked(s->encoder->SubmitInput(surface),"submit current frame");
        auto deadline=GetTickCount64()+2000;
        while(GetTickCount64()<deadline) {
            amf::AMFDataPtr output;
            AMF_RESULT result=s->encoder->QueryOutput(&output);
            if(output) {
                s->output=amf::AMFBufferPtr(output);
                if(!s->output)throw std::runtime_error("encoder returned no bitstream buffer");
                *bytes=(const unsigned char*)s->output->GetNative();*length=s->output->GetSize();*timestamp=s->output->GetPts();
                return 0;
            }
            if(result!=AMF_OK && result!=AMF_REPEAT && result!=AMF_NEED_MORE_INPUT)checked(result,"query current frame");
            SwitchToThread();
        }
        throw std::runtime_error("AMF did not complete the current frame within two seconds");
    } catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}
}
