// FP16 scRGB composition for a single native video window. MIT OR Apache-2.0.
#include "d3d11_hdr_display.h"
#include <d3d11.h>
#include <d3d10.h>
#include <dcomp.h>
#include <dxgi1_5.h>
#include <wrl/client.h>
#include <cstdio>
#include <stdexcept>
#include <memory>
#include <string>
using Microsoft::WRL::ComPtr;
namespace {
void check(HRESULT hr,const char* operation){if(FAILED(hr))throw std::runtime_error(std::string(operation)+": HRESULT="+std::to_string((unsigned long)hr));}
struct Apartment {
    HRESULT result=CoInitializeEx(nullptr,COINIT_APARTMENTTHREADED);
    Apartment(){if(FAILED(result)&&result!=RPC_E_CHANGED_MODE)check(result,"initialize composition apartment");}
    ~Apartment(){if(SUCCEEDED(result))CoUninitialize();}
};
struct Presentation {
    Apartment apartment;
    HWND window;
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    ComPtr<IDXGIFactory2> factory;
    ComPtr<IDXGIFactory1> inventory;
    ComPtr<EclipseOutput6> active_output;
    ComPtr<IDXGISwapChain3> chain;
    ComPtr<ID3D11Texture2D> shared_texture;
    ComPtr<IDCompositionDevice> composition;
    ComPtr<IDCompositionTarget> target;
    ComPtr<IDCompositionVisual> visual;
    unsigned width=0,height=0;
    bool hdr=false;
    bool allow_tearing=false;
    float white=80;
    HMONITOR monitor=nullptr;
    ULONGLONG queried=0;
    Presentation(HWND hwnd):window(hwnd) {
        if(!IsWindow(hwnd))throw std::runtime_error("HDR presentation requires a live native window");
        check(CreateDXGIFactory1(__uuidof(IDXGIFactory1),(void**)inventory.GetAddressOf()),"create HDR inventory");
        ComPtr<IDXGIAdapter1> selected;
        HMONITOR active=MonitorFromWindow(hwnd,MONITOR_DEFAULTTONEAREST);
        for(UINT ai=0;ai<32&&!selected;ai++) {
            ComPtr<IDXGIAdapter1> adapter;if(inventory->EnumAdapters1(ai,&adapter)==DXGI_ERROR_NOT_FOUND)break;
            for(UINT oi=0;oi<32;oi++) {
                ComPtr<IDXGIOutput> output;if(adapter->EnumOutputs(oi,&output)==DXGI_ERROR_NOT_FOUND)break;
                DXGI_OUTPUT_DESC desc{};if(SUCCEEDED(output->GetDesc(&desc))&&desc.AttachedToDesktop&&desc.Monitor==active){selected=adapter;break;}
            }
        }
        if(!selected)throw std::runtime_error("no adapter owns the HDR window's output");
        D3D_FEATURE_LEVEL level=D3D_FEATURE_LEVEL_11_0;
        check(D3D11CreateDevice(selected.Get(),D3D_DRIVER_TYPE_UNKNOWN,nullptr,D3D11_CREATE_DEVICE_BGRA_SUPPORT,&level,1,D3D11_SDK_VERSION,&device,nullptr,&context),"create HDR presentation device");
        ComPtr<ID3D10Multithread> threading;check(context.As(&threading),"query HDR presentation threading");threading->SetMultithreadProtected(TRUE);
        check(selected->GetParent(__uuidof(IDXGIFactory2),(void**)factory.GetAddressOf()),"get HDR composition factory");
        ComPtr<IDXGIFactory5> modern;
        BOOL supported=FALSE;
        if(SUCCEEDED(factory.As(&modern)) && SUCCEEDED(modern->CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING,&supported,sizeof(supported))))allow_tearing=supported!=FALSE;
        refresh(true);
        if(!hdr)throw std::runtime_error("the current display output has HDR disabled");
        ComPtr<IDXGIDevice> dxgi;check(device.As(&dxgi),"query HDR DXGI device");
        check(DCompositionCreateDevice(dxgi.Get(),__uuidof(IDCompositionDevice),(void**)composition.GetAddressOf()),"create HDR composition device");
        check(composition->CreateTargetForHwnd(window,TRUE,&target),"create HDR window composition target");
        check(composition->CreateVisual(&visual),"create HDR composition visual");
        // Content is attached only after GL registration and the first frame.
    }
    ~Presentation(){if(target&&composition){target->SetRoot(nullptr);composition->Commit();}}
    void refresh(bool force=false) {
        auto now=GetTickCount64();auto active=MonitorFromWindow(window,MONITOR_DEFAULTTONEAREST);
        if(!force&&active==monitor&&now-queried<500)return;
        bool changed=active!=monitor;monitor=active;queried=now;hdr=false;white=80;
        if(!inventory || !inventory->IsCurrent()) {
            inventory.Reset();active_output.Reset();changed=true;
            if(FAILED(CreateDXGIFactory1(__uuidof(IDXGIFactory1),(void**)inventory.GetAddressOf())))return;
        }
        if(changed||!active_output) {
            active_output.Reset();
            for(UINT ai=0;ai<32&&!active_output;ai++) {
                ComPtr<IDXGIAdapter1> adapter;HRESULT result=inventory->EnumAdapters1(ai,&adapter);if(result==DXGI_ERROR_NOT_FOUND)break;if(FAILED(result))continue;
                for(UINT oi=0;oi<32;oi++) {
                    ComPtr<IDXGIOutput> output;result=adapter->EnumOutputs(oi,&output);if(result==DXGI_ERROR_NOT_FOUND)break;if(FAILED(result))continue;
                    ComPtr<EclipseOutput6> advanced;if(FAILED(output->QueryInterface(eclipse_output6_iid,(void**)advanced.GetAddressOf())))continue;
                    EclipseOutputDesc1 desc{};if(SUCCEEDED(advanced->GetDesc1(&desc))&&desc.attached&&desc.monitor==active){active_output=advanced;break;}
                }
            }
        }
        EclipseOutputDesc1 desc{};
        if(active_output && SUCCEEDED(active_output->GetDesc1(&desc))) {
            hdr=desc.color==DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
            white=hdr?eclipse_sdr_white(desc.name):80.f;
        }
    }

    void resize(unsigned w,unsigned h) {
        if(!w||!h||w>16384||h>16384)throw std::runtime_error("invalid HDR window size");
        if(width==w&&height==h)return;
        // Caller has unregistered the old WGL object before this replacement.
        shared_texture.Reset();
        if(!chain) {
            DXGI_SWAP_CHAIN_DESC1 desc{};desc.Width=w;desc.Height=h;desc.Format=DXGI_FORMAT_R16G16B16A16_FLOAT;
            desc.SampleDesc.Count=1;desc.BufferUsage=DXGI_USAGE_RENDER_TARGET_OUTPUT;desc.BufferCount=2;
            desc.SwapEffect=DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL;desc.AlphaMode=DXGI_ALPHA_MODE_PREMULTIPLIED;
            desc.Scaling=DXGI_SCALING_STRETCH;
            desc.Flags=allow_tearing?DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING:0;
            ComPtr<IDXGISwapChain1> initial;
            HRESULT created=factory->CreateSwapChainForComposition(device.Get(),&desc,nullptr,&initial);
            if(allow_tearing&&(created==DXGI_ERROR_INVALID_CALL||created==DXGI_ERROR_UNSUPPORTED)) {
                allow_tearing=false;desc.Flags=0;
                created=factory->CreateSwapChainForComposition(device.Get(),&desc,nullptr,&initial);
            }
            check(created,"create FP16 HDR swap chain");
            std::fprintf(stderr,"HDR composition allow_tearing=%s buffers=2\n",allow_tearing?"true":"false");
            check(initial.As(&chain),"query HDR swap chain");
            check(chain->SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709),"set scRGB HDR color space");
            ComPtr<IDXGIDevice1> latency;check(device.As(&latency),"query HDR frame latency");check(latency->SetMaximumFrameLatency(1),"limit HDR frame latency");
        }else check(chain->ResizeBuffers(2,w,h,DXGI_FORMAT_R16G16B16A16_FLOAT,allow_tearing?DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING:0),"resize HDR swap chain");
        D3D11_TEXTURE2D_DESC td{};td.Width=w;td.Height=h;td.MipLevels=1;td.ArraySize=1;td.Format=DXGI_FORMAT_R16G16B16A16_FLOAT;
        td.SampleDesc.Count=1;td.Usage=D3D11_USAGE_DEFAULT;td.BindFlags=D3D11_BIND_RENDER_TARGET|D3D11_BIND_SHADER_RESOURCE;td.MiscFlags=D3D11_RESOURCE_MISC_SHARED;
        check(device->CreateTexture2D(&td,nullptr,&shared_texture),"create HDR GL composition texture");
        width=w;height=h;
    }
    void deactivate() {
        if(visual_attached){check(target->SetRoot(nullptr),"detach HDR composition");check(composition->Commit(),"commit HDR deactivation");visual_attached=false;}
    }
    void present(bool vsync) {
        if(!chain||!shared_texture)throw std::runtime_error("HDR presentation surface is not ready");
        ComPtr<ID3D11Texture2D> backbuffer;check(chain->GetBuffer(0,__uuidof(ID3D11Texture2D),(void**)backbuffer.GetAddressOf()),"get HDR backbuffer");
        context->CopyResource(backbuffer.Get(),shared_texture.Get());
        check(chain->Present(vsync?1:0,!vsync&&allow_tearing?DXGI_PRESENT_ALLOW_TEARING:0),"present scRGB HDR frame");
        if(!visual_attached) {
            check(visual->SetContent(chain.Get()),"attach HDR swap chain");check(target->SetRoot(visual.Get()),"attach HDR composition visual");
            check(composition->Commit(),"commit HDR composition");visual_attached=true;
        }
    }
    bool visual_attached=false;
};
}
extern "C" float eclipse_hdr_desktop_white(const WCHAR* name){return eclipse_sdr_white(name);}
extern "C" void* eclipse_hdr_present_open(void* hwnd,char* error,size_t capacity) {
    try{return new Presentation((HWND)hwnd);}catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return nullptr;}
}
extern "C" void eclipse_hdr_present_close(void* value){delete (Presentation*)value;}
extern "C" void* eclipse_hdr_present_device(void* value){return ((Presentation*)value)->device.Get();}
extern "C" void* eclipse_hdr_present_texture(void* value){return ((Presentation*)value)->shared_texture.Get();}
extern "C" int eclipse_hdr_present_resize(void* value,unsigned w,unsigned h,char* error,size_t capacity) {
    try{((Presentation*)value)->resize(w,h);return 0;}catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}
}
extern "C" void eclipse_hdr_present_info(void* value,int* hdr,float* white) {auto p=(Presentation*)value;p->refresh();*hdr=p->hdr?1:0;*white=p->white;}
extern "C" int eclipse_hdr_present_frame(void* value,int vsync,char* error,size_t capacity) {
    try{((Presentation*)value)->present(vsync!=0);return 0;}catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}
}

extern "C" int eclipse_hdr_present_deactivate(void* value,char* error,size_t capacity) {
    try{((Presentation*)value)->deactivate();return 0;}catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}
}
