// GPU HDR color conversion. Written for Eclipse; MIT OR Apache-2.0.
// ST 2084 and BT.2020 coefficients describe the signal, independent of codecs.
#include <windows.h>
#include <d3d11.h>
#include <d3d10.h>
#include <d3dcompiler.h>
#include <wrl/client.h>
#include <cstdio>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>
using Microsoft::WRL::ComPtr;
namespace {
void check(HRESULT result,const char* operation) {
    if(FAILED(result))throw std::runtime_error(std::string(operation)+": HRESULT="+std::to_string((unsigned long)result));
}
const char shader[]=R"(
Texture2D<float4> image:register(t0);
Texture2D<float2> chroma:register(t1);
SamplerState bilinear:register(s0);
cbuffer Params:register(b0) { float4 dimensions; float4 options; };
float4 vertex(uint id:SV_VertexID):SV_Position {
    float2 p=float2((id<<1)&2,id&2);return float4(p.x*2-1,1-p.y*2,0,1);
}
float3 pq_encode(float3 nits) {
    float3 p=pow(saturate(nits/10000.0),2610.0/16384.0);
    return pow((3424.0/4096.0+(2413.0/128.0)*p)/(1+(2392.0/128.0)*p),2523.0/32.0);
}
float3 pq_decode(float3 signal) {
    float3 p=pow(saturate(signal),32.0/2523.0);
    return 10000*pow(max(p-3424.0/4096.0,0)/(2413.0/128.0-(2392.0/128.0)*p),16384.0/2610.0);
}
float3 srgb_encode(float3 value) {
    return float3(value.r<=0.0031308?value.r*12.92:1.055*pow(value.r,1.0/2.4)-0.055,
        value.g<=0.0031308?value.g*12.92:1.055*pow(value.g,1.0/2.4)-0.055,
        value.b<=0.0031308?value.b*12.92:1.055*pow(value.b,1.0/2.4)-0.055);
}
float3 srgb_decode(float3 value) {
    return float3(value.r<=0.04045?value.r/12.92:pow((value.r+0.055)/1.055,2.4),
        value.g<=0.04045?value.g/12.92:pow((value.g+0.055)/1.055,2.4),
        value.b<=0.04045?value.b/12.92:pow((value.b+0.055)/1.055,2.4));
}
float3 signal_at(float2 uv) {
    float3 linear_rgb=image.SampleLevel(bilinear,uv,0).rgb;
    if(options.z>0.5)linear_rgb=srgb_decode(linear_rgb)*options.w/80;
    if(options.y>0.5) {
        float3 wide=mul(float3x3(0.627404,0.329283,0.043313,0.069097,0.919540,0.011362,0.016391,0.088013,0.895595),linear_rgb);
        return pq_encode(wide*80);
    }
    // SDR fallback has a continuous highlight shoulder; never clamp scRGB
    // to 8 bits before applying the source desktop's configured white level.
    linear_rgb=max(linear_rgb*80/options.w,0);
    float peak=max(linear_rgb.r,max(linear_rgb.g,linear_rgb.b));
    if(peak>0.75) linear_rgb=(linear_rgb/peak)*(1-0.25*exp(-4*(peak-0.75)));
    return srgb_encode(linear_rgb);
}
float4 encode(float4 position:SV_Position):SV_Target {
    float3 rgb;
    if(options.x<0.5) rgb=signal_at(position.xy/dimensions.xy);
    else {
        float2 uv=(position.xy*2-0.5)/dimensions.xy;
        float2 step=1/dimensions.xy;
        rgb=(signal_at(uv)+signal_at(uv+float2(step.x,0))+signal_at(uv+float2(0,step.y))+signal_at(uv+step))*0.25;
    }
    bool hdr=options.y>0.5;
    float3 weights=hdr?float3(0.2627,0.6780,0.0593):float3(0.299,0.587,0.114);
    float y=dot(rgb,weights);
    float2 uv=float2((rgb.b-y)/(2*(1-weights.b)),(rgb.r-y)/(2*(1-weights.r)));
    if(hdr) {
        float yy=floor(clamp(64+876*y,64,940)+0.5)*64/65535.0;
        float2 cc=floor(clamp(512+896*uv,64,960)+0.5)*64/65535.0;
        return options.x<0.5?float4(yy,0,0,1):float4(cc,0,1);
    }
    return options.x<0.5?float4((16+219*y)/255,0,0,1):float4((128+224*uv)/255,0,1);
}
float4 decode(float4 position:SV_Position):SV_Target {
    float2 coord=position.xy/dimensions.zw;
    float y=image.SampleLevel(bilinear,coord,0).r*65535/64;
    float2 uv=chroma.SampleLevel(bilinear,coord,0)*65535/64;
    y=(y-(options.z>0.5?0:64))/(options.z>0.5?1023:876);
    uv=(uv-512)/(options.z>0.5?1023:896);
    float3 rgb=float3(y+1.4746*uv.y,y-0.164553*uv.x-0.571353*uv.y,y+1.8814*uv.x);
    rgb=pq_decode(rgb)/80;
    rgb=mul(float3x3(1.660491,-0.587641,-0.072850,-0.124550,1.132900,-0.008349,-0.018151,-0.100579,1.118730),rgb);
    return float4(rgb,1);
}
)";
struct Color {
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    ComPtr<ID3D10Multithread> threading;
    ComPtr<ID3D11VertexShader> vs;
    ComPtr<ID3D11PixelShader> ps;
    ComPtr<ID3D11Buffer> params;
    ComPtr<ID3D11SamplerState> sampler;
    ComPtr<ID3D11Texture2D> source,output;
    ComPtr<ID3D11ShaderResourceView> y,uv;
    ComPtr<ID3D11RenderTargetView> target;
    ComPtr<ID3D11RasterizerState> rasterizer;
    ComPtr<ID3D11DepthStencilState> depth;
    unsigned width,height;
    bool decoder,hdr,shared;
    float white;
    Color(ID3D11Device* native,unsigned w,unsigned h,bool decoding,bool high,bool exportable,float sdr_white)
        :device(native),width(w),height(h),decoder(decoding),hdr(high),shared(exportable),white(sdr_white) {
        if(!native||w<2||h<2||w>8192||h>8192||w%2||h%2||sdr_white<40||sdr_white>1000)throw std::runtime_error("invalid HDR converter layout");
        device->GetImmediateContext(&context);
        check(context.As(&threading),"query HDR context threading");threading->SetMultithreadProtected(TRUE);
        ComPtr<ID3DBlob> vertex,pixel,errors;
        check(D3DCompile(shader,sizeof(shader)-1,nullptr,nullptr,nullptr,"vertex","vs_5_0",D3DCOMPILE_OPTIMIZATION_LEVEL3,0,&vertex,&errors),"compile HDR vertex shader");
        HRESULT result=D3DCompile(shader,sizeof(shader)-1,nullptr,nullptr,nullptr,decoder?"decode":"encode","ps_5_0",D3DCOMPILE_OPTIMIZATION_LEVEL3,0,&pixel,&errors);
        if(FAILED(result)&&errors)throw std::runtime_error(std::string((const char*)errors->GetBufferPointer(),errors->GetBufferSize()));
        check(result,"compile HDR pixel shader");
        check(device->CreateVertexShader(vertex->GetBufferPointer(),vertex->GetBufferSize(),nullptr,&vs),"create HDR vertex shader");
        check(device->CreatePixelShader(pixel->GetBufferPointer(),pixel->GetBufferSize(),nullptr,&ps),"create HDR pixel shader");
        D3D11_BUFFER_DESC bd{};bd.ByteWidth=32;bd.Usage=D3D11_USAGE_DEFAULT;bd.BindFlags=D3D11_BIND_CONSTANT_BUFFER;
        check(device->CreateBuffer(&bd,nullptr,&params),"create HDR parameters");
        D3D11_SAMPLER_DESC sd{};sd.Filter=D3D11_FILTER_MIN_MAG_LINEAR_MIP_POINT;sd.AddressU=sd.AddressV=sd.AddressW=D3D11_TEXTURE_ADDRESS_CLAMP;sd.MaxLOD=D3D11_FLOAT32_MAX;
        check(device->CreateSamplerState(&sd,&sampler),"create HDR sampler");
        D3D11_RASTERIZER_DESC rd{};rd.FillMode=D3D11_FILL_SOLID;rd.CullMode=D3D11_CULL_NONE;rd.DepthClipEnable=TRUE;
        check(device->CreateRasterizerState(&rd,&rasterizer),"create HDR rasterizer");
        D3D11_DEPTH_STENCIL_DESC dd{};dd.DepthEnable=FALSE;dd.StencilEnable=FALSE;
        check(device->CreateDepthStencilState(&dd,&depth),"create HDR depth state");
        if(decoder){output=texture(DXGI_FORMAT_R16G16B16A16_FLOAT,w,h,shared);check(device->CreateRenderTargetView(output.Get(),nullptr,&target),"create HDR RGB output view");}
    }
    ComPtr<ID3D11Texture2D> texture(DXGI_FORMAT format,unsigned w,unsigned h,bool exportable) {
        D3D11_TEXTURE2D_DESC td{};td.Width=w;td.Height=h;td.ArraySize=1;td.MipLevels=1;td.Format=format;td.SampleDesc.Count=1;
        td.Usage=D3D11_USAGE_DEFAULT;td.BindFlags=D3D11_BIND_SHADER_RESOURCE|D3D11_BIND_RENDER_TARGET;td.MiscFlags=exportable?D3D11_RESOURCE_MISC_SHARED:0;
        ComPtr<ID3D11Texture2D> value;check(device->CreateTexture2D(&td,nullptr,&value),"create HDR texture");return value;
    }
    void draw(ID3D11RenderTargetView* destination,unsigned w,unsigned h,const float* values) {
        context->UpdateSubresource(params.Get(),0,nullptr,values,0,0);
        context->IASetInputLayout(nullptr);context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        context->VSSetShader(vs.Get(),nullptr,0);context->PSSetShader(ps.Get(),nullptr,0);
        context->GSSetShader(nullptr,nullptr,0);context->HSSetShader(nullptr,nullptr,0);context->DSSetShader(nullptr,nullptr,0);
        ID3D11Buffer* buffer=params.Get();context->PSSetConstantBuffers(0,1,&buffer);
        ID3D11SamplerState* sampling=sampler.Get();context->PSSetSamplers(0,1,&sampling);
        context->OMSetBlendState(nullptr,nullptr,~0u);context->OMSetDepthStencilState(depth.Get(),0);context->RSSetState(rasterizer.Get());
        D3D11_VIEWPORT viewport{0,0,(float)w,(float)h,0,1};context->RSSetViewports(1,&viewport);
        context->OMSetRenderTargets(1,&destination,nullptr);context->Draw(3,0);context->OMSetRenderTargets(0,nullptr,nullptr);
    }
    ComPtr<ID3D11Texture2D> convert(ID3D11Texture2D* input,unsigned index,bool full) {
        struct Lock {ID3D10Multithread* p;Lock(ID3D10Multithread* v):p(v){p->Enter();}~Lock(){p->Leave();}} lock(threading.Get());
        D3D11_TEXTURE2D_DESC td{};input->GetDesc(&td);
        ComPtr<ID3D11Device> owner;input->GetDevice(&owner);
        if(owner.Get()!=device.Get()||index>=td.ArraySize||td.MipLevels!=1||td.SampleDesc.Count!=1||td.Width<width||td.Height<height)throw std::runtime_error("HDR source device/layout mismatch");
        float values[8]={(float)width,(float)height,(float)td.Width,(float)td.Height,0,hdr?1.f:0.f,full?1.f:0.f,white};
        ComPtr<ID3D11ShaderResourceView> input_view;
        if(decoder) {
            if(td.Format!=DXGI_FORMAT_P010)throw std::runtime_error("HDR decoder requires P010");
            D3D11_TEXTURE2D_DESC old{};if(source)source->GetDesc(&old);
            if(!source||old.Width!=td.Width||old.Height!=td.Height) {
                y.Reset();uv.Reset();source=texture(DXGI_FORMAT_P010,td.Width,td.Height,false);
                D3D11_SHADER_RESOURCE_VIEW_DESC vd{};vd.ViewDimension=D3D11_SRV_DIMENSION_TEXTURE2D;vd.Texture2D.MipLevels=1;
                vd.Format=DXGI_FORMAT_R16_UNORM;check(device->CreateShaderResourceView(source.Get(),&vd,&y),"create P010 luma view");
                vd.Format=DXGI_FORMAT_R16G16_UNORM;check(device->CreateShaderResourceView(source.Get(),&vd,&uv),"create P010 chroma view");
            }
            context->CopySubresourceRegion(source.Get(),0,0,0,0,input,index,nullptr);
            ID3D11ShaderResourceView* views[]={y.Get(),uv.Get()};context->PSSetShaderResources(0,2,views);
            draw(target.Get(),width,height,values);
        } else {
            if((td.Format!=DXGI_FORMAT_R16G16B16A16_FLOAT && td.Format!=DXGI_FORMAT_B8G8R8A8_UNORM) || index!=0)throw std::runtime_error("HDR capture requires linear_rgb FP16 scRGB");
            values[6]=td.Format==DXGI_FORMAT_B8G8R8A8_UNORM?1.f:0.f;
            check(device->CreateShaderResourceView(input,nullptr,&input_view),"create RGB source view");
            ID3D11ShaderResourceView* view=input_view.Get();context->PSSetShaderResources(0,1,&view);
            output=texture(hdr?DXGI_FORMAT_P010:DXGI_FORMAT_NV12,width,height,false);
            for(unsigned plane=0;plane<2;plane++) {
                D3D11_RENDER_TARGET_VIEW_DESC vd{};vd.ViewDimension=D3D11_RTV_DIMENSION_TEXTURE2D;
                vd.Format=hdr?(plane?DXGI_FORMAT_R16G16_UNORM:DXGI_FORMAT_R16_UNORM):(plane?DXGI_FORMAT_R8G8_UNORM:DXGI_FORMAT_R8_UNORM);
                ComPtr<ID3D11RenderTargetView> out;check(device->CreateRenderTargetView(output.Get(),&vd,&out),"create encoder plane view");
                values[4]=(float)plane;draw(out.Get(),plane?width/2:width,plane?height/2:height,values);
            }
        }
        ID3D11ShaderResourceView* none[2]={};context->PSSetShaderResources(0,2,none);context->Flush();return output;
    }
};
}
extern "C" void* eclipse_color_open(void* device,unsigned width,unsigned height,int decoder,int hdr,int shared,float white,char* error,size_t capacity) {
    try{return new Color((ID3D11Device*)device,width,height,decoder!=0,hdr!=0,shared!=0,white);}
    catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return nullptr;}
}
extern "C" void eclipse_color_close(void* value){delete (Color*)value;}
extern "C" void* eclipse_color_output(void* value){return ((Color*)value)->output.Get();}
extern "C" void* eclipse_color_convert(void* value,void* source,unsigned index,int full,char* error,size_t capacity) {
    try{auto output=((Color*)value)->convert((ID3D11Texture2D*)source,index,full!=0);return output.Detach();}
    catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return nullptr;}
}
