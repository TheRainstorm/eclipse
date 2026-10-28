// FP16 scRGB composition for a single native video window. MIT OR Apache-2.0.
#include "d3d11_hdr_display.h"
#include <d3d11.h>
#include <d3d11_4.h>
#include <d3dcompiler.h>
#include <array>
#include <vector>
#include <cstring>
#include <cstdlib>
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
// Keep DComp and the WGL drawable on different HWNDs. A fullscreen WGL window
// can otherwise stop DWM from consuming the composition chain. This child has
// no GL drawable or redirection bitmap and always passes input to its parent.
struct CompositionWindow {
    HWND value=nullptr;
    static LRESULT CALLBACK procedure(HWND window,UINT message,WPARAM w,LPARAM l) {
        if(message==WM_NCCREATE) {
            SetWindowLongPtrW(window,GWLP_USERDATA,(LONG_PTR)((CREATESTRUCTW*)l)->lpCreateParams);
        }else if(message==WM_NCDESTROY) {
            auto owner=(CompositionWindow*)GetWindowLongPtrW(window,GWLP_USERDATA);
            if(owner)owner->value=nullptr;
        }
        if(message==WM_NCHITTEST)return HTTRANSPARENT;
        if(message==WM_MOUSEACTIVATE)return MA_NOACTIVATE;
        return DefWindowProcW(window,message,w,l);
    }
    explicit CompositionWindow(HWND parent) {
        // HTTRANSPARENT only routes through windows on the same UI thread.
        if(!IsWindow(parent)||GetWindowThreadProcessId(parent,nullptr)!=GetCurrentThreadId())
            throw std::runtime_error("native composition requires its parent's UI thread");
        static const ATOM registered=[] {
            WNDCLASSW definition{};
            definition.lpfnWndProc=procedure;
            definition.hInstance=GetModuleHandleW(nullptr);
            definition.lpszClassName=L"EclipseNativeCompositionSurface";
            return RegisterClassW(&definition);
        }();
        if(!registered)throw std::runtime_error("register native composition window");
        value=CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP|WS_EX_NOACTIVATE|WS_EX_TRANSPARENT,
            L"EclipseNativeCompositionSurface",L"",WS_CHILD|WS_CLIPSIBLINGS,
            0,0,1,1,parent,nullptr,GetModuleHandleW(nullptr),this);
        if(!value)throw std::runtime_error("create native composition window");
    }
    CompositionWindow(const CompositionWindow&)=delete;
    CompositionWindow& operator=(const CompositionWindow&)=delete;
    ~CompositionWindow(){if(value)DestroyWindow(value);}
    void resize(unsigned width,unsigned height) {
        if(value&&!SetWindowPos(value,HWND_TOP,0,0,width,height,SWP_NOACTIVATE))
            throw std::runtime_error("resize native composition window");
    }
    void visible(bool show){if(value)ShowWindow(value,show?SW_SHOWNOACTIVATE:SW_HIDE);}
};
struct Presentation {
    Apartment apartment;
    CompositionWindow content_window;
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
    Presentation(HWND hwnd,bool allow_sdr=false):content_window(hwnd),window(hwnd) {
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
        if(!hdr&&!allow_sdr)throw std::runtime_error("the current display output has HDR disabled");
        ComPtr<IDXGIDevice> dxgi;check(device.As(&dxgi),"query HDR DXGI device");
        check(DCompositionCreateDevice(dxgi.Get(),__uuidof(IDCompositionDevice),(void**)composition.GetAddressOf()),"create HDR composition device");
        check(composition->CreateTargetForHwnd(content_window.value,TRUE,&target),"create HDR window composition target");
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

    void resize(unsigned w,unsigned h,bool allocate_shared=true) {
        if(!w||!h||w>16384||h>16384)throw std::runtime_error("invalid HDR window size");
        if(width==w&&height==h)return;
        content_window.resize(w,h);
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
        if(allocate_shared)check(device->CreateTexture2D(&td,nullptr,&shared_texture),"create HDR GL composition texture");
        width=w;height=h;
    }
    void deactivate() {
        content_window.visible(false);
        if(visual_attached){check(target->SetRoot(nullptr),"detach HDR composition");check(composition->Commit(),"commit HDR deactivation");visual_attached=false;}
    }
    void present(bool vsync) {
        if(!chain||!shared_texture)throw std::runtime_error("HDR presentation surface is not ready");
        ComPtr<ID3D11Texture2D> backbuffer;check(chain->GetBuffer(0,__uuidof(ID3D11Texture2D),(void**)backbuffer.GetAddressOf()),"get HDR backbuffer");
        context->CopyResource(backbuffer.Get(),shared_texture.Get());
        check(chain->Present(vsync?1:0,!vsync&&allow_tearing?DXGI_PRESENT_ALLOW_TEARING:0),"present scRGB HDR frame");
        attach();
    }
    void attach() {
        if(!visual_attached) {
            check(visual->SetContent(chain.Get()),"attach HDR swap chain");check(target->SetRoot(visual.Get()),"attach HDR composition visual");
            check(composition->Commit(),"commit HDR composition");visual_attached=true;
            content_window.visible(true);
        }
    }
    bool visual_attached=false;
};
struct NativePresentation:Presentation {
    std::array<ComPtr<ID3D11Texture2D>,3> overlays;
    std::array<ComPtr<ID3D11ShaderResourceView>,3> overlay_views;
    ComPtr<ID3D11Device5> modern;
    ComPtr<ID3D11DeviceContext4> commands;
    ComPtr<ID3D11Texture2D> image;
    ComPtr<ID3D11ShaderResourceView> image_view;
    ComPtr<ID3D11Fence> ready,consumed;
    ComPtr<ID3D11VertexShader> vertex;
    ComPtr<ID3D11PixelShader> pixel;
    ComPtr<ID3D11Buffer> parameters;
    ComPtr<ID3D11SamplerState> linear,nearest;
    ComPtr<ID3D11RasterizerState> rasterizer;
    std::array<ComPtr<ID3D11Texture2D>,2> cursors;
    std::array<ComPtr<ID3D11ShaderResourceView>,2> cursor_views;
    unsigned cursor_width=0,cursor_height=0;
    uint64_t source_id=0,ready_value=0,return_value=0;
    bool offered=false;uint64_t submitted_return=0;
    bool debug_enabled=false;int debug_hdr=-1;float debug_white=0;uint64_t debug_serial=0;
    ComPtr<ID3D11Texture2D> debug_target;std::vector<uint16_t> debug_pixels;
    unsigned debug_width=0,debug_height=0;
    struct alignas(16) Params {float rect[4]{},output[4]{},flags[4]{},cursor[4]{};} params;
    explicit NativePresentation(HWND hwnd):Presentation(hwnd,true) {
        check(device.As(&modern),"query native presentation fence device");check(context.As(&commands),"query native presentation fence context");
        std::fprintf(stderr,"{\"event\":\"native_composition\",\"ui_slots\":3}\n");
        const char shader[]=R"(
Texture2D<float4> video:register(t0);Texture2D<float4> overlay:register(t1);
Texture2D<float4> cursor_alpha:register(t2);Texture2D<float4> cursor_xor:register(t3);
SamplerState bilinear:register(s0);SamplerState point_sample:register(s1);
cbuffer Params:register(b0){float4 rect;float4 output;float4 flags;float4 cursor;}
float4 vs(uint id:SV_VertexID):SV_Position{float2 p=float2((id<<1)&2,id&2);return float4(p.x*2-1,1-p.y*2,0,1);}
float3 gamma_rgb(float3 v){return sign(v)*lerp(abs(v)*12.92,1.055*pow(abs(v),1.0/2.4)-0.055,step(0.0031308,abs(v)));}
float3 linear_rgb(float3 v){return sign(v)*lerp(abs(v)/12.92,pow((abs(v)+0.055)/1.055,2.4),step(0.04045,abs(v)));}
float4 ps(float4 pos:SV_Position):SV_Target{
 float2 uv=(pos.xy-rect.xy)/max(rect.zw,1);float3 base=0;
 if(flags.z>0 && all(uv>=0) && all(uv<1)){
  float3 rgb=video.SampleLevel(bilinear,uv,0).rgb;
  if(flags.x>0){
   if(flags.y>0)rgb=gamma_rgb(rgb/output.z);
   else{rgb=max(rgb*80/output.w,0);float peak=max(rgb.r,max(rgb.g,rgb.b));if(peak>0.75)rgb=rgb/peak*(1-0.25*exp(-4*(peak-0.75)));rgb=gamma_rgb(rgb);}
  }
  if(flags.w>0){float2 c=(uv-cursor.xy)/cursor.zw;
   if(all(c>=0)&&all(c<1)){float4 a=cursor_alpha.SampleLevel(point_sample,c,0);rgb=lerp(rgb,a.rgb,a.a);uint3 x=(uint3)round(cursor_xor.SampleLevel(point_sample,c,0).rgb*255);if(any(x!=0))rgb=float3(((uint3)round(saturate(rgb)*255))^x)/255;}
  }
  base=linear_rgb(rgb)*output.z;
 }
 float2 screen=pos.xy/output.xy;float4 ui=overlay.SampleLevel(bilinear,float2(screen.x,1-screen.y),0);
 // Match egui's gamma-space premultiplied blending, including additive
 // colors with zero alpha. Never unpremultiply gamma channels as linear RGB.
 float3 composite=ui.rgb+gamma_rgb(base/output.z)*(1-ui.a);
 return float4(linear_rgb(composite)*output.z,1);
})";
        ComPtr<ID3DBlob> vs,ps,error;
        HRESULT result=D3DCompile(shader,sizeof(shader),nullptr,nullptr,nullptr,"vs","vs_5_0",D3DCOMPILE_OPTIMIZATION_LEVEL3,0,&vs,&error);
        if(FAILED(result)&&error)throw std::runtime_error((const char*)error->GetBufferPointer());check(result,"compile native composition vertex shader");
        error.Reset();result=D3DCompile(shader,sizeof(shader),nullptr,nullptr,nullptr,"ps","ps_5_0",D3DCOMPILE_OPTIMIZATION_LEVEL3,0,&ps,&error);
        if(FAILED(result)&&error)throw std::runtime_error((const char*)error->GetBufferPointer());check(result,"compile native composition pixel shader");
        check(device->CreateVertexShader(vs->GetBufferPointer(),vs->GetBufferSize(),nullptr,&vertex),"create native composition vertex shader");
        check(device->CreatePixelShader(ps->GetBufferPointer(),ps->GetBufferSize(),nullptr,&pixel),"create native composition pixel shader");
        D3D11_BUFFER_DESC bd{};bd.ByteWidth=sizeof(Params);bd.Usage=D3D11_USAGE_DEFAULT;bd.BindFlags=D3D11_BIND_CONSTANT_BUFFER;check(device->CreateBuffer(&bd,nullptr,&parameters),"create native composition parameters");
        D3D11_SAMPLER_DESC sd{};sd.AddressU=sd.AddressV=sd.AddressW=D3D11_TEXTURE_ADDRESS_CLAMP;sd.MaxLOD=D3D11_FLOAT32_MAX;sd.Filter=D3D11_FILTER_MIN_MAG_LINEAR_MIP_POINT;
        check(device->CreateSamplerState(&sd,&linear),"create native video sampler");sd.Filter=D3D11_FILTER_MIN_MAG_MIP_POINT;check(device->CreateSamplerState(&sd,&nearest),"create native cursor sampler");
        D3D11_RASTERIZER_DESC rd{};rd.FillMode=D3D11_FILL_SOLID;rd.CullMode=D3D11_CULL_NONE;rd.DepthClipEnable=TRUE;check(device->CreateRasterizerState(&rd,&rasterizer),"create native composition rasterizer");
    }
    void resize(unsigned w,unsigned h){
        if(width==w&&height==h)return;
        for(auto& view:overlay_views)view.Reset();for(auto& texture:overlays)texture.Reset();
        debug_target.Reset();debug_pixels.clear();
        Presentation::resize(w,h,false);
        D3D11_TEXTURE2D_DESC td{};td.Width=w;td.Height=h;td.MipLevels=1;td.ArraySize=1;td.Format=DXGI_FORMAT_R16G16B16A16_FLOAT;td.SampleDesc.Count=1;
        td.Usage=D3D11_USAGE_DEFAULT;td.BindFlags=D3D11_BIND_RENDER_TARGET|D3D11_BIND_SHADER_RESOURCE;td.MiscFlags=D3D11_RESOURCE_MISC_SHARED;
        for(unsigned i=0;i<3;i++){
            check(device->CreateTexture2D(&td,nullptr,&overlays[i]),"create native UI ring texture");
            ComPtr<ID3D11Texture2D> sampled=overlays[i];
            check(device->CreateShaderResourceView(sampled.Get(),nullptr,&overlay_views[i]),"create native UI ring view");
        }
    }
    void offer(uint64_t id,void* const* handles,uint64_t produced_value,uint64_t consumed_value,const float* rect,bool source_hdr,float source_white){
        if(source_id!=id){
            ComPtr<ID3D11Texture2D> next_image;ComPtr<ID3D11Fence> next_ready,next_consumed;ComPtr<ID3D11ShaderResourceView> next_view;
            check(modern->OpenSharedResource1(handles[0],IID_PPV_ARGS(&next_image)),"open native video image");
            check(modern->OpenSharedFence(handles[1],IID_PPV_ARGS(&next_ready)),"open native video ready fence");
            check(modern->OpenSharedFence(handles[2],IID_PPV_ARGS(&next_consumed)),"open native video return fence");
            check(device->CreateShaderResourceView(next_image.Get(),nullptr,&next_view),"create native video view");
            image=next_image;image_view=next_view;ready=next_ready;consumed=next_consumed;source_id=id;
        }
        offered=true;ready_value=produced_value;return_value=consumed_value;
        for(unsigned i=0;i<4;i++)params.rect[i]=rect[i];params.flags[0]=source_hdr?1:0;params.output[3]=source_white;
    }
    void cursor_shape(unsigned w,unsigned h,const unsigned char* alpha,const unsigned char* xored){
        if(!w||!h||w>512||h>512)throw std::runtime_error("invalid native cursor dimensions");
        if(w!=cursor_width||h!=cursor_height){
            D3D11_TEXTURE2D_DESC td{};td.Width=w;td.Height=h;td.MipLevels=td.ArraySize=1;td.Format=DXGI_FORMAT_R8G8B8A8_UNORM;td.SampleDesc.Count=1;td.Usage=D3D11_USAGE_DEFAULT;td.BindFlags=D3D11_BIND_SHADER_RESOURCE;
            for(unsigned i=0;i<2;i++){cursor_views[i].Reset();cursors[i].Reset();check(device->CreateTexture2D(&td,nullptr,&cursors[i]),"create native cursor texture");check(device->CreateShaderResourceView(cursors[i].Get(),nullptr,&cursor_views[i]),"create native cursor view");}cursor_width=w;cursor_height=h;
        }
        context->UpdateSubresource(cursors[0].Get(),0,nullptr,alpha,w*4,0);context->UpdateSubresource(cursors[1].Get(),0,nullptr,xored,w*4,0);
    }
    void cursor_position(const float* rect){params.flags[3]=rect?1:0;if(rect)for(unsigned i=0;i<4;i++)params.cursor[i]=rect[i];}
    void deactivate(){
        Presentation::deactivate();
        image_view.Reset();image.Reset();ready.Reset();consumed.Reset();source_id=0;
        offered=false;submitted_return=0;params.flags[3]=0;
        for(auto& v:cursor_views)v.Reset();for(auto& c:cursors)c.Reset();cursor_width=cursor_height=0;
    }
    void begin(){offered=false;params.flags[3]=0;}
    void present(unsigned index,bool vsync){
        submitted_return=0;
        if(index>=3||!chain)throw std::runtime_error("native presentation ring is not ready");
        ComPtr<ID3D11Texture2D> backbuffer;check(chain->GetBuffer(0,IID_PPV_ARGS(&backbuffer)),"get native video backbuffer");
        ComPtr<ID3D11RenderTargetView> target;check(device->CreateRenderTargetView(backbuffer.Get(),nullptr,&target),"create native video target");
        if(offered)check(commands->Wait(ready.Get(),ready_value),"GPU wait for converted native video");
        params.output[0]=float(width);params.output[1]=float(height);params.output[2]=(debug_enabled&&debug_white>0?debug_white:white)/80;params.flags[1]=(debug_enabled&&debug_hdr>=0?debug_hdr!=0:hdr)?1:0;params.flags[2]=offered?1:0;
        context->UpdateSubresource(parameters.Get(),0,nullptr,&params,0,0);
        context->OMSetRenderTargets(1,target.GetAddressOf(),nullptr);context->OMSetBlendState(nullptr,nullptr,0xffffffff);context->OMSetDepthStencilState(nullptr,0);context->RSSetState(rasterizer.Get());
        D3D11_VIEWPORT viewport{0,0,float(width),float(height),0,1};context->RSSetViewports(1,&viewport);context->IASetInputLayout(nullptr);context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        context->VSSetShader(vertex.Get(),nullptr,0);context->PSSetShader(pixel.Get(),nullptr,0);context->PSSetConstantBuffers(0,1,parameters.GetAddressOf());
        ID3D11ShaderResourceView* views[4]={offered?image_view.Get():nullptr,overlay_views[index].Get(),cursor_views[0].Get(),cursor_views[1].Get()};context->PSSetShaderResources(0,4,views);
        ID3D11SamplerState* samplers[2]={linear.Get(),nearest.Get()};context->PSSetSamplers(0,2,samplers);context->Draw(3,0);
        ID3D11ShaderResourceView* none[4]={};context->PSSetShaderResources(0,4,none);context->OMSetRenderTargets(0,nullptr,nullptr);
        if(offered){check(commands->Signal(consumed.Get(),return_value),"return native video image to producer");submitted_return=return_value;}
        if(debug_enabled){
            // Explicit generated-image probe only. Production never allocates a
            // staging buffer or copies a video frame to the CPU.
            if(width>2048||height>2048)throw std::runtime_error("native pixel probe target too large");
            if(!debug_target){D3D11_TEXTURE2D_DESC td{};backbuffer->GetDesc(&td);td.Usage=D3D11_USAGE_STAGING;td.BindFlags=td.MiscFlags=0;td.CPUAccessFlags=D3D11_CPU_ACCESS_READ;check(device->CreateTexture2D(&td,nullptr,&debug_target),"create native pixel probe staging");}
            context->CopyResource(debug_target.Get(),backbuffer.Get());D3D11_MAPPED_SUBRESOURCE map{};check(context->Map(debug_target.Get(),0,D3D11_MAP_READ,0,&map),"read generated native composition");
            debug_pixels.resize(size_t(width)*height*4);for(unsigned y=0;y<height;y++)std::memcpy(debug_pixels.data()+size_t(y)*width*4,(const unsigned char*)map.pData+size_t(y)*map.RowPitch,size_t(width)*8);context->Unmap(debug_target.Get(),0);
            debug_width=width;debug_height=height;debug_serial++;
        }
        check(chain->Present(vsync?1:0,!vsync&&allow_tearing?DXGI_PRESENT_ALLOW_TEARING:0),"present native composed video");attach();
    }
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

extern "C" void* eclipse_native_present_open(void* hwnd,char* error,size_t capacity){try{return new NativePresentation((HWND)hwnd);}catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return nullptr;}}
extern "C" void eclipse_native_present_close(void* value){delete (NativePresentation*)value;}
extern "C" void* eclipse_native_present_texture(void* value,unsigned index){return index<3?((NativePresentation*)value)->overlays[index].Get():nullptr;}
extern "C" int eclipse_native_present_resize(void* value,unsigned w,unsigned h,char* error,size_t capacity){try{((NativePresentation*)value)->resize(w,h);return 0;}catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}}
extern "C" void eclipse_native_present_begin(void* value){((NativePresentation*)value)->begin();}
extern "C" int eclipse_native_present_image(void* value,uint64_t id,void* const* handles,uint64_t ready,uint64_t consumed,const float* rect,int hdr,float white,char* error,size_t capacity){try{((NativePresentation*)value)->offer(id,handles,ready,consumed,rect,hdr!=0,white);return 0;}catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}}
extern "C" int eclipse_native_present_cursor(void* value,unsigned w,unsigned h,const unsigned char* alpha,const unsigned char* xored,char* error,size_t capacity){try{((NativePresentation*)value)->cursor_shape(w,h,alpha,xored);return 0;}catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}}
extern "C" void eclipse_native_present_cursor_position(void* value,const float* rect){((NativePresentation*)value)->cursor_position(rect);}
extern "C" int eclipse_native_present_frame(void* value,unsigned index,int vsync,uint64_t* submitted,char* error,size_t capacity){auto p=(NativePresentation*)value;try{p->present(index,vsync!=0);*submitted=p->submitted_return;return 0;}catch(const std::exception& e){p->context->Flush();*submitted=p->submitted_return;if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}}

extern "C" void eclipse_native_present_debug(void* value,int enabled,int hdr,float white){auto p=(NativePresentation*)value;p->debug_enabled=enabled!=0;p->debug_hdr=hdr;p->debug_white=white;if(!enabled){p->debug_target.Reset();p->debug_pixels.clear();}}
extern "C" size_t eclipse_native_present_debug_pixels(void* value,uint16_t* data,size_t capacity,unsigned* w,unsigned* h,uint64_t* serial){auto p=(NativePresentation*)value;*w=p->debug_width;*h=p->debug_height;*serial=p->debug_serial;if(data&&capacity>=p->debug_pixels.size())std::memcpy(data,p->debug_pixels.data(),p->debug_pixels.size()*2);return p->debug_pixels.size();}

extern "C" void* eclipse_native_present_device(void* value){return ((NativePresentation*)value)->device.Get();}

extern "C" int eclipse_native_present_deactivate(void* value,char* error,size_t capacity){try{((NativePresentation*)value)->deactivate();return 0;}catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return -1;}}
extern "C" void eclipse_native_present_info(void* value,int* hdr,float* white){auto p=(NativePresentation*)value;p->refresh();*hdr=p->hdr?1:0;*white=p->white;}
