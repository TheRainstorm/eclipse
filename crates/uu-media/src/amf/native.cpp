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
#include <atomic>
#include <cstdlib>
#include <array>
#include <chrono>
#include <sstream>
#include <algorithm>
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
struct LeaseStats {
    std::atomic<unsigned long long> submitted{0},released{0},duplicates{0};
};
// One observer per submission, never rebound to another slot or generation.
// The callback retains itself and its texture independently of Session/Rust.
struct InputLease : amf::AMFSurfaceObserver, std::enable_shared_from_this<InputLease> {
    std::atomic<int> state{0}; // fresh, armed, released
    std::shared_ptr<InputLease> keepalive;
    std::shared_ptr<LeaseStats> stats;
    ID3D11Texture2D* texture=nullptr;
    ~InputLease(){if(texture)texture->Release();}
    void release() {
        auto hold=shared_from_this();
        if(state.exchange(2,std::memory_order_acq_rel)==2) {if(stats)stats->duplicates++;return;}
        if(stats)stats->released++;
        keepalive.reset();
    }
    void AMF_STD_CALL OnSurfaceDataRelease(amf::AMFSurface*) override {release();}
};
using LeaseHandle=std::shared_ptr<InputLease>;
// Fixed memory and no I/O on the encode path. These are CPU/API wall times,
// including driver waits, not GPU execution times. Quantiles are bucket bounds.
struct ApiTime {
    unsigned long long count=0,total_ns=0,max_ns=0;
    std::array<unsigned long long,2049> buckets{};
    void add(unsigned long long ns) {
        ++count;total_ns+=ns;max_ns=std::max(max_ns,ns);
        ++buckets[std::min<unsigned long long>(ns/25000,2048)];
    }
    double percentile(unsigned numerator)const {
        auto rank=(count*numerator+99)/100,seen=0ull;
        for(size_t i=0;i<buckets.size();++i) {
            seen+=buckets[i];if(seen>=rank)return i==2048?max_ns/1000.0:(i+1)*25.0;
        }
        return 0;
    }
    void json(std::ostream& out)const {
        out<<"{\"samples\":"<<count<<",\"mean_us\":"<<(count?total_ns/1000.0/count:0)
           <<",\"max_us\":"<<max_ns/1000.0<<",\"p50_upper_us\":"<<(count?percentile(50):0)
           <<",\"p95_upper_us\":"<<(count?percentile(95):0)<<",\"p99_upper_us\":"<<(count?percentile(99):0)
           <<",\"over_51200_us\":"<<buckets.back()<<"}";
    }
};
static unsigned long long monotonic_ns() {
    return std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now().time_since_epoch()).count();
}
struct NativeProfile {
    bool enabled=std::getenv("UU_ENCODE_PROFILE")!=nullptr;
    ApiTime surface,wrap_api,submit,query_api,wait_wall,yield_api,total,query_call;
    unsigned long long queries=0,ok=0,repeat=0,need_input=0,other=0,yields=0,yield_switched=0,outputs=0,errors=0,pts_mismatch=0;
    unsigned long long encoded_bytes=0,first_output_bytes=0,first_output_ns=0,last_output_ns=0;
    void print()const {
        if(!enabled)return;
        std::ostringstream out;
        out<<"{\"event\":\"amf_native_profile\",\"scope\":\"CPU/API monotonic wall time; 25us histogram upper bounds, not GPU time\",\"surface\":";
        surface.json(out);out<<",\"wrap_dx11_api\":";wrap_api.json(out);out<<",\"submit\":";submit.json(out);out<<",\"query_api_per_frame\":";query_api.json(out);
        out<<",\"output_wait_wall\":";wait_wall.json(out);out<<",\"yield_api_per_frame\":";yield_api.json(out);
        out<<",\"native_encode_total\":";total.json(out);out<<",\"query_api_per_call\":";query_call.json(out);
        out<<",\"queries\":"<<queries<<",\"query_ok\":"<<ok<<",\"query_repeat\":"<<repeat<<",\"query_need_input\":"<<need_input
           <<",\"query_other\":"<<other<<",\"yields\":"<<yields<<",\"yield_switched\":"<<yield_switched
           <<",\"outputs\":"<<outputs<<",\"errors\":"<<errors<<",\"pts_mismatch\":"<<pts_mismatch
           <<",\"encoded_bytes\":"<<encoded_bytes<<",\"first_output_bytes\":"<<first_output_bytes
           <<",\"output_span_us\":"<<(last_output_ns-first_output_ns)/1000.0<<"}\n";
        auto line=out.str();std::fwrite(line.data(),1,line.size(),stderr);
    }
};
struct ProfileCall {
    NativeProfile& p;
    unsigned long long start=0,wait_start=0,wait_end=0,query_ns=0,yield_ns=0;
    bool success=false;
    explicit ProfileCall(NativeProfile& profile):p(profile) {if(p.enabled)start=monotonic_ns();}
    ~ProfileCall() {
        if(!p.enabled)return;
        auto end=monotonic_ns();p.total.add(end-start);p.query_api.add(query_ns);p.yield_api.add(yield_ns);
        if(wait_start)p.wait_wall.add((wait_end?wait_end:end)-wait_start);
        if(!success)++p.errors;
    }
};
extern "C" void* eclipse_amf_lease_new() {
    try{return new LeaseHandle(std::make_shared<InputLease>());}catch(...){return nullptr;}
}
extern "C" int eclipse_amf_lease_released(void* value) {
    return (*(LeaseHandle*)value)->state.load(std::memory_order_acquire)!=1;
}
extern "C" void eclipse_amf_lease_close(void* value) {delete (LeaseHandle*)value;}
struct Session {
    HMODULE library=nullptr;
    amf::AMFContextPtr context;
    amf::AMFComponentPtr encoder;
    amf::AMFBufferPtr output;
    ID3D11Device* device=nullptr;
    int codec=0;
    unsigned width=0,height=0,fps=0;
    std::shared_ptr<LeaseStats> leases=std::make_shared<LeaseStats>();
    NativeProfile profile;
    ~Session() {
        output=nullptr;
        if(encoder) encoder->Terminate();
        encoder=nullptr;
        if(context) context->Terminate();
        context=nullptr;
        profile.print();
        if(std::getenv("UU_ENCODE_PROFILE"))std::fprintf(stderr,"uu-amf-leases submitted=%llu released=%llu pending=%llu duplicates=%llu\n",
            leases->submitted.load(),leases->released.load(),leases->submitted.load()-leases->released.load(),leases->duplicates.load());
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
    void log_property(const char* label,const wchar_t* name)const {
        amf::AMFVariant value;
        auto result=encoder->GetProperty(name,static_cast<amf::AMFVariantStruct*>(&value));
        std::fprintf(stderr,"{\"event\":\"amf_property\",\"name\":\"%s\",\"result\":%d,\"supported\":%s,\"value\":",
            label,int(result),result==AMF_OK?"true":"false");
        if(result==AMF_OK && (value.type==amf::AMF_VARIANT_INT64 || value.type==amf::AMF_VARIANT_BOOL))
            std::fprintf(stderr,"%lld",(long long)value.ToInt64());
        else std::fputs("null",stderr);
        std::fputs("}\n",stderr);
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
    unsigned peak,int vbr,int quality,int hdr,int default_adapter,void* native_device,char* error,size_t capacity) {
    try {
        if((hdr&&codec==0)||codec<0||codec>2||quality<0||quality>2||width<2||height<2||!fps||!kbps)throw std::runtime_error("invalid AMF settings");
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
        s->set(s->prop(AMF_VIDEO_ENCODER_INPUT_COLOR_PROFILE,AMF_VIDEO_ENCODER_HEVC_INPUT_COLOR_PROFILE,AMF_VIDEO_ENCODER_AV1_INPUT_COLOR_PROFILE),amf_int64(hdr?AMF_VIDEO_CONVERTER_COLOR_PROFILE_2020:AMF_VIDEO_CONVERTER_COLOR_PROFILE_601));
        s->set(s->prop(AMF_VIDEO_ENCODER_OUTPUT_COLOR_PROFILE,AMF_VIDEO_ENCODER_HEVC_OUTPUT_COLOR_PROFILE,AMF_VIDEO_ENCODER_AV1_OUTPUT_COLOR_PROFILE),amf_int64(hdr?AMF_VIDEO_CONVERTER_COLOR_PROFILE_2020:AMF_VIDEO_CONVERTER_COLOR_PROFILE_601));
        s->set(s->prop(AMF_VIDEO_ENCODER_OUTPUT_COLOR_PRIMARIES,AMF_VIDEO_ENCODER_HEVC_OUTPUT_COLOR_PRIMARIES,AMF_VIDEO_ENCODER_AV1_OUTPUT_COLOR_PRIMARIES),amf_int64(hdr?AMF_COLOR_PRIMARIES_BT2020:AMF_COLOR_PRIMARIES_SMPTE170M));
        s->set(s->prop(AMF_VIDEO_ENCODER_OUTPUT_TRANSFER_CHARACTERISTIC,AMF_VIDEO_ENCODER_HEVC_OUTPUT_TRANSFER_CHARACTERISTIC,AMF_VIDEO_ENCODER_AV1_OUTPUT_TRANSFER_CHARACTERISTIC),amf_int64(hdr?AMF_COLOR_TRANSFER_CHARACTERISTIC_SMPTE2084:AMF_COLOR_TRANSFER_CHARACTERISTIC_BT709));
        if(hdr) {
            s->set(codec==1?AMF_VIDEO_ENCODER_HEVC_COLOR_BIT_DEPTH:AMF_VIDEO_ENCODER_AV1_COLOR_BIT_DEPTH,amf_int64(AMF_COLOR_BIT_DEPTH_10));
            if(codec==1)s->set(AMF_VIDEO_ENCODER_HEVC_PROFILE,amf_int64(AMF_VIDEO_ENCODER_HEVC_PROFILE_MAIN_10));
        }
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
        // Opt-in experiment only. Keep the production wait policy as default.
        const char* timeout_env=std::getenv("UU_AMF_QUERY_TIMEOUT_MS");
        if(timeout_env && std::strcmp(timeout_env,"0") && std::strcmp(timeout_env,"1"))throw std::runtime_error("UU_AMF_QUERY_TIMEOUT_MS must be 0 or 1");
        amf_int64 query_timeout=timeout_env && !std::strcmp(timeout_env,"0")?0:1;
        const auto timeout_name=s->prop(AMF_VIDEO_ENCODER_QUERY_TIMEOUT,AMF_VIDEO_ENCODER_HEVC_QUERY_TIMEOUT,AMF_VIDEO_ENCODER_AV1_QUERY_TIMEOUT);
        auto timeout_result=s->encoder->SetProperty(timeout_name,query_timeout);
        // AV1-only experiment: capacity is not a count of queued frames. The
        // unset path retains the driver's policy without an extra property set.
        const char* queue_env=codec==2?std::getenv("UU_AMF_AV1_INPUT_QUEUE_SIZE"):nullptr;
        amf_int64 requested_queue=0;
        if(queue_env) {
            if(std::strcmp(queue_env,"1") && std::strcmp(queue_env,"16"))throw std::runtime_error("UU_AMF_AV1_INPUT_QUEUE_SIZE must be 1 or 16");
            requested_queue=!std::strcmp(queue_env,"1")?1:16;
            auto result=s->encoder->SetProperty(AMF_VIDEO_ENCODER_AV1_INPUT_QUEUE_SIZE,requested_queue);
            std::fprintf(stderr,"{\"event\":\"amf_av1_input_queue\",\"phase\":\"before_init\",\"requested\":%lld,\"set_result\":%d}\n",(long long)requested_queue,int(result));
            checked(result,"set experimental AV1 input queue capacity");
        }
        checked(s->encoder->Init(hdr?amf::AMF_SURFACE_P010:amf::AMF_SURFACE_NV12,width,height),"initialize encoder");
        if(queue_env) {
            amf_int64 actual_queue=-1;
            auto result=s->encoder->GetProperty(AMF_VIDEO_ENCODER_AV1_INPUT_QUEUE_SIZE,&actual_queue);
            std::fprintf(stderr,"{\"event\":\"amf_av1_input_queue\",\"phase\":\"after_init\",\"requested\":%lld,\"get_result\":%d,\"actual\":%lld}\n",(long long)requested_queue,int(result),(long long)actual_queue);
            checked(result,"read experimental AV1 input queue capacity");
            if(actual_queue!=requested_queue)throw std::runtime_error("AMF changed the experimental AV1 input queue capacity");
        }
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
        if(s->profile.enabled) {
            amf_int64 actual_timeout=-1;auto timeout_read=s->encoder->GetProperty(timeout_name,&actual_timeout);
            std::fprintf(stderr,"{\"event\":\"amf_init\",\"codec\":%d,\"runtime_version\":%llu,\"sdk_version\":%llu,\"query_timeout_requested_ms\":%lld,\"query_timeout_set_result\":%d,\"query_timeout_get_result\":%d,\"query_timeout_actual_ms\":%lld}\n",
                codec,(unsigned long long)version,(unsigned long long)AMF_FULL_VERSION,(long long)query_timeout,int(timeout_result),int(timeout_read),(long long)actual_timeout);
            s->log_property("usage",s->prop(AMF_VIDEO_ENCODER_USAGE,AMF_VIDEO_ENCODER_HEVC_USAGE,AMF_VIDEO_ENCODER_AV1_USAGE));
            s->log_property("quality",quality_name);s->log_property("query_timeout_ms",timeout_name);
            s->log_property("input_queue_size",s->prop(AMF_VIDEO_ENCODER_INPUT_QUEUE_SIZE,AMF_VIDEO_ENCODER_HEVC_INPUT_QUEUE_SIZE,AMF_VIDEO_ENCODER_AV1_INPUT_QUEUE_SIZE));
            s->log_property("preanalysis",s->prop(AMF_VIDEO_ENCODER_PRE_ANALYSIS_ENABLE,AMF_VIDEO_ENCODER_HEVC_PRE_ANALYSIS_ENABLE,AMF_VIDEO_ENCODER_AV1_PRE_ANALYSIS_ENABLE));
            s->log_property("gop",gop_name);
            s->log_property("rate_control",s->prop(AMF_VIDEO_ENCODER_RATE_CONTROL_METHOD,AMF_VIDEO_ENCODER_HEVC_RATE_CONTROL_METHOD,AMF_VIDEO_ENCODER_AV1_RATE_CONTROL_METHOD));
            s->log_property("target_bitrate",s->prop(AMF_VIDEO_ENCODER_TARGET_BITRATE,AMF_VIDEO_ENCODER_HEVC_TARGET_BITRATE,AMF_VIDEO_ENCODER_AV1_TARGET_BITRATE));
            s->log_property("peak_bitrate",s->prop(AMF_VIDEO_ENCODER_PEAK_BITRATE,AMF_VIDEO_ENCODER_HEVC_PEAK_BITRATE,AMF_VIDEO_ENCODER_AV1_PEAK_BITRATE));
            if(codec==2) {
                s->log_property("latency_mode",AMF_VIDEO_ENCODER_AV1_ENCODING_LATENCY_MODE);
                s->log_property("b_pictures",AMF_VIDEO_ENCODER_AV1_B_PIC_PATTERN);
            } else if(codec==0)s->log_property("b_pictures",AMF_VIDEO_ENCODER_B_PIC_PATTERN);
        }
        return s.release();
    } catch(const std::exception& e) {if(capacity)std::snprintf(error,capacity,"%s",e.what());return nullptr;}
}
extern "C" unsigned long long eclipse_amf_close(void* session) {
    auto stats=((Session*)session)->leases;
    delete (Session*)session;
    return stats->submitted.load()-stats->released.load();
}
extern "C" void* eclipse_amf_device(void* session) {return ((Session*)session)->device;}
extern "C" int eclipse_amf_bitrate(void* session,unsigned kbps,char* error,size_t capacity) {
    try {((Session*)session)->bitrate(kbps);return 0;}
    catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}
}
extern "C" int eclipse_amf_encode(void* session,void* native_texture,const unsigned char* cpu_nv12,
    size_t cpu_length,long long pts,int keyframe,void* lease_handle,const unsigned char** bytes,size_t* length,long long* timestamp,char* error,size_t capacity) {
    try {
        auto s=(Session*)session;s->output=nullptr;ProfileCall profile(s->profile);
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
            LeaseHandle lease;
            if(lease_handle) {
                lease=*(LeaseHandle*)lease_handle;
                int fresh=0;
                if(!lease->state.compare_exchange_strong(fresh,1))throw std::runtime_error("AMF input lease already submitted");
                lease->texture=texture;texture->AddRef();lease->stats=s->leases;
                lease->keepalive=lease;s->leases->submitted++;
            }
            auto wrap_start=s->profile.enabled?monotonic_ns():0;
            auto wrapped=s->context->CreateSurfaceFromDX11Native(texture,&surface,lease.get());
            if(s->profile.enabled)s->profile.wrap_api.add(monotonic_ns()-wrap_start);
            if(wrapped!=AMF_OK && lease && !surface)lease->release();
            checked(wrapped,"wrap DX11 surface");
        }
        if(s->profile.enabled)s->profile.surface.add(monotonic_ns()-profile.start);
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
        auto submit_start=s->profile.enabled?monotonic_ns():0;
        auto submitted=s->encoder->SubmitInput(surface);
        if(s->profile.enabled) {profile.wait_start=monotonic_ns();s->profile.submit.add(profile.wait_start-submit_start);}
        checked(submitted,"submit current frame");
        auto deadline=GetTickCount64()+2000;
        while(GetTickCount64()<deadline) {
            amf::AMFDataPtr output;
            auto query_start=s->profile.enabled?monotonic_ns():0;
            AMF_RESULT result=s->encoder->QueryOutput(&output);
            if(s->profile.enabled) {
                auto ns=monotonic_ns()-query_start;profile.query_ns+=ns;s->profile.query_call.add(ns);++s->profile.queries;
                if(result==AMF_OK)++s->profile.ok;else if(result==AMF_REPEAT)++s->profile.repeat;
                else if(result==AMF_NEED_MORE_INPUT)++s->profile.need_input;else ++s->profile.other;
            }
            if(output) {
                if(s->profile.enabled)profile.wait_end=monotonic_ns();
                s->output=amf::AMFBufferPtr(output);
                if(!s->output)throw std::runtime_error("encoder returned no bitstream buffer");
                *bytes=(const unsigned char*)s->output->GetNative();*length=s->output->GetSize();*timestamp=s->output->GetPts();
                profile.success=true;
                if(s->profile.enabled){
                    if(!s->profile.outputs){s->profile.first_output_bytes=*length;s->profile.first_output_ns=profile.wait_end;}
                    s->profile.last_output_ns=profile.wait_end;s->profile.encoded_bytes+=*length;
                    ++s->profile.outputs;if(*timestamp!=pts)++s->profile.pts_mismatch;
                }
                return 0;
            }
            if(result!=AMF_OK && result!=AMF_REPEAT && result!=AMF_NEED_MORE_INPUT)checked(result,"query current frame");
            auto yield_start=s->profile.enabled?monotonic_ns():0;
            auto switched=SwitchToThread();
            if(s->profile.enabled){profile.yield_ns+=monotonic_ns()-yield_start;++s->profile.yields;if(switched)++s->profile.yield_switched;}
        }
        throw std::runtime_error("AMF did not complete the current frame within two seconds");
    } catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}
}
