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
#include <array>
#include <cstdlib>
#include <cmath>
#include <chrono>
#include <algorithm>
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
float3 linear_at(float2 uv) {
    uv=clamp(uv,0.5/dimensions.xy,1-0.5/dimensions.xy);
    float3 linear_rgb=image.SampleLevel(bilinear,uv,0).rgb;
    if(options.z>0.5)linear_rgb=srgb_decode(linear_rgb)*options.w/80;
    return linear_rgb;
}
float3 signal_from_linear(float3 linear_rgb) {
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
float3 signal_at(float2 uv) {return signal_from_linear(linear_at(uv));}
float4 encode(float4 position:SV_Position):SV_Target {
    float3 rgb;
    if(options.x<0.5) rgb=signal_at(position.xy/dimensions.xy);
    else {
        // LEFT 4:2:0: co-sited horizontally, centered between the two rows.
        // Filter in linear light before PQ so bright subpixel text cannot
        // alias into saturated chroma when reducing to half resolution.
        float2 center=float2(position.x*2-0.5,position.y*2)/dimensions.xy;
        float2 step=1/dimensions.xy;
        float2 top=center-float2(0,step.y*0.5),bottom=center+float2(0,step.y*0.5);
        float3 filtered=(linear_at(top)+linear_at(bottom))*0.25;
        filtered+=(linear_at(top-float2(step.x,0))+linear_at(bottom-float2(step.x,0))
            +linear_at(top+float2(step.x,0))+linear_at(bottom+float2(step.x,0)))*0.125;
        rgb=signal_from_linear(filtered);
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
// SDR desktop samples stay in their original nonlinear RGB signal. LEFT
// 4:2:0 places chroma at the left luma column and between the two rows.
// A horizontal [1/4, 1/2, 1/4] tent limits subpixel text aliasing before decimation.
float3 sdr_signal_at(float2 uv) {
    uv=clamp(uv,0.5/dimensions.xy,1-0.5/dimensions.xy);
    return options.z>0.5?image.SampleLevel(bilinear,uv,0).rgb:signal_at(uv);
}
float4 encode_sdr(float4 position:SV_Position):SV_Target {
    float3 rgb;
    if(options.x<0.5)rgb=sdr_signal_at(position.xy/dimensions.xy);
    else {
        float2 center=float2(position.x*2-0.5,position.y*2)/dimensions.xy;
        float2 step=1/dimensions.xy;
        float2 top=center-float2(0,step.y*0.5);
        float2 bottom=center+float2(0,step.y*0.5);
        rgb=(sdr_signal_at(top)+sdr_signal_at(bottom))*0.25;
        rgb+=(sdr_signal_at(top-float2(step.x,0))+sdr_signal_at(bottom-float2(step.x,0))
            +sdr_signal_at(top+float2(step.x,0))+sdr_signal_at(bottom+float2(step.x,0)))*0.125;
    }
    float y=dot(rgb,float3(0.299,0.587,0.114));
    float2 uv=float2((rgb.b-y)/1.772,(rgb.r-y)/1.402);
    return options.x<0.5?float4((16+219*y)/255,0,0,1):float4((128+224*uv)/255,0,1);
}
float4 decode_sdr(float4 position:SV_Position):SV_Target {
    float2 coord=position.xy/dimensions.zw;
    // MPEG-2/LEFT 4:2:0: chroma is horizontally co-sited with luma.
    // Bound both planes to visible samples, excluding codec alignment rows.
    float2 uv_coord=clamp(coord+float2(0.5/dimensions.z,0),1/dimensions.zw,(dimensions.xy-1)/dimensions.zw);
    float y=image.SampleLevel(bilinear,coord,0).r*255;
    float2 uv=chroma.SampleLevel(bilinear,uv_coord,0)*255;
    bool full=options.z>0.5;
    y=(y-(full?0:16))/(full?255:219);
    uv=(uv-128)/(full?255:224);
    float4 c=options.x>0.5?float4(1.5748,0.187324,0.468124,1.8556):float4(1.402,0.344136,0.714136,1.772);
    return float4(saturate(float3(y+c.x*uv.y,y-c.y*uv.x-c.z*uv.y,y+c.w*uv.x)),1);
}
float4 decode(float4 position:SV_Position):SV_Target {
    float2 coord=clamp(position.xy/dimensions.zw,0.5/dimensions.zw,(dimensions.xy-0.5)/dimensions.zw);
    float2 uv_coord=clamp(coord+float2(0.5/dimensions.z,0),1/dimensions.zw,(dimensions.xy-1)/dimensions.zw);
    float y=image.SampleLevel(bilinear,coord,0).r*65535/64;
    float2 uv=chroma.SampleLevel(bilinear,uv_coord,0)*65535/64;
    y=(y-(options.z>0.5?0:64))/(options.z>0.5?1023:876);
    uv=(uv-512)/(options.z>0.5?1023:896);
    float3 rgb=float3(y+1.4746*uv.y,y-0.164553*uv.x-0.571353*uv.y,y+1.8814*uv.x);
    rgb=pq_decode(rgb)/80;
    rgb=mul(float3x3(1.660491,-0.587641,-0.072850,-0.124550,1.132900,-0.008349,-0.018151,-0.100579,1.118730),rgb);
    return float4(rgb,1);
}
)";
struct EncodeTarget {
    ComPtr<ID3D11Texture2D> output;
    std::array<ComPtr<ID3D11RenderTargetView>,2> planes;
};
// Optional bounded diagnostics: elapsed conversion commands, not AMF/video-engine
// time or time queued before the first timestamp. No per-frame I/O or GPU wait.
struct GpuProfile {
    struct Slot {ComPtr<ID3D11Query> disjoint,start,end;bool pending=false,failed=false;};
    std::array<Slot,8> slots;
    std::array<unsigned long long,4097> histogram{}; // 25us bounds; last is overflow
    unsigned long long samples=0,valid=0,not_ready=0,disjoint=0,skipped=0,errors=0,unsupported=0,cpu_count=0;
    double total_ms=0,max_ms=0,cpu_total_ms=0,cpu_max_ms=0;
    bool available=true;
    explicit GpuProfile(ID3D11Device* device) {
        for(auto& slot:slots) {
            D3D11_QUERY_DESC d{D3D11_QUERY_TIMESTAMP_DISJOINT,0};
            HRESULT result=device->CreateQuery(&d,&slot.disjoint);
            d.Query=D3D11_QUERY_TIMESTAMP;
            if(SUCCEEDED(result))result=device->CreateQuery(&d,&slot.start);
            if(SUCCEEDED(result))result=device->CreateQuery(&d,&slot.end);
            if(FAILED(result)) {available=false;if(result==DXGI_ERROR_UNSUPPORTED||result==E_NOTIMPL)++unsupported;else ++errors;break;}
        }
    }
    void collect(ID3D11DeviceContext* context) {
        if(!available)return;
        for(auto& slot:slots) {
            if(!slot.pending||slot.failed)continue;
            D3D11_QUERY_DATA_TIMESTAMP_DISJOINT data{};UINT64 start=0,end=0;
            HRESULT result=context->GetData(slot.disjoint.Get(),&data,sizeof(data),D3D11_ASYNC_GETDATA_DONOTFLUSH);
            if(result==S_OK)result=context->GetData(slot.start.Get(),&start,sizeof(start),D3D11_ASYNC_GETDATA_DONOTFLUSH);
            if(result==S_OK)result=context->GetData(slot.end.Get(),&end,sizeof(end),D3D11_ASYNC_GETDATA_DONOTFLUSH);
            if(result==S_FALSE){++not_ready;continue;} // one attempt/slot/frame, never poll
            if(result!=S_OK){++errors;slot.failed=true;continue;} // do not reuse unknown query
            slot.pending=false;
            if(data.Disjoint){++disjoint;continue;}
            if(!data.Frequency||end<start){++errors;continue;}
            const double ms=double(end-start)*1000.0/double(data.Frequency);
            ++valid;total_ms+=ms;max_ms=std::max(max_ms,ms);
            const size_t bucket=ms>=102.4?4096:size_t(std::ceil(ms/0.025));++histogram[bucket];
        }
    }
    Slot* begin(ID3D11DeviceContext* context) {
        if(available)for(auto& slot:slots)if(!slot.pending&&!slot.failed) {
            slot.pending=true;++samples;context->Begin(slot.disjoint.Get());context->End(slot.start.Get());return &slot;
        }
        ++skipped;return nullptr;
    }
    void end(ID3D11DeviceContext* context,Slot* slot) {
        if(slot){context->End(slot->end.Get());context->End(slot->disjoint.Get());}
    }
    void cpu(double ms){++cpu_count;cpu_total_ms+=ms;cpu_max_ms=std::max(cpu_max_ms,ms);}
    double percentile(unsigned p) const {
        if(!valid)return -1;
        const auto target=(valid*p+99)/100;unsigned long long count=0;
        for(size_t i=0;i<histogram.size();++i){count+=histogram[i];if(count>=target)return double(i)*0.025;}
        return -1;
    }
    ~GpuProfile() {
        unsigned pending=0;for(const auto& slot:slots)if(slot.pending)++pending;
        std::fprintf(stderr,"{\"event\":\"gpu_color_profile\",\"scope\":\"conversion command GPU elapsed only; 25us quantile upper bounds; last bucket overflow; CPU includes original Flush; no PTS readiness attribution\",\"ring_size\":8,\"samples\":%llu,\"valid\":%llu,\"not_ready_polls\":%llu,\"disjoint\":%llu,\"skipped\":%llu,\"errors\":%llu,\"unsupported\":%llu,\"uncollected\":%u,\"mean_ms\":%.6f,\"p50_ms\":%.6f,\"p95_ms\":%.6f,\"p99_ms\":%.6f,\"max_ms\":%.6f,\"cpu_calls\":%llu,\"cpu_mean_ms\":%.6f,\"cpu_max_ms\":%.6f}\n",
            samples,valid,not_ready,disjoint,skipped,errors,unsupported,pending,valid?total_ms/valid:-1.,percentile(50),percentile(95),percentile(99),valid?max_ms:-1.,cpu_count,cpu_count?cpu_total_ms/cpu_count:-1.,cpu_count?cpu_max_ms:-1.);
    }
};
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
    bool decoder,hdr;unsigned shared;
    float white;
    struct SourceView {ComPtr<ID3D11Texture2D> texture;ComPtr<ID3D11ShaderResourceView> view;};
    std::array<SourceView,2> cached_sources;
    unsigned next_source=0;
    std::unique_ptr<GpuProfile> profile;
    unsigned long long pooled_frames=0,fresh_frames=0,target_allocations=0,source_view_allocations=0;
    ~Color() {
        if(!decoder && std::getenv("UU_ENCODE_PROFILE"))std::fprintf(stderr,
            "uu-color-input pooled_frames=%llu fresh_frames=%llu target_allocations=%llu source_view_allocations=%llu output=%s\n",
            pooled_frames,fresh_frames,target_allocations,source_view_allocations,hdr?"P010":"NV12");
    }
    Color(ID3D11Device* native,unsigned w,unsigned h,bool decoding,bool high,unsigned exportable,float sdr_white)
        :device(native),width(w),height(h),decoder(decoding),hdr(high),shared(exportable),white(sdr_white) {
        if(!native||w<2||h<2||w>8192||h>8192||w%2||h%2||sdr_white<40||sdr_white>1000)throw std::runtime_error("invalid GPU color converter layout");
        device->GetImmediateContext(&context);
        check(context.As(&threading),"query HDR context threading");threading->SetMultithreadProtected(TRUE);
        ComPtr<ID3DBlob> vertex,pixel,errors;
        check(D3DCompile(shader,sizeof(shader)-1,nullptr,nullptr,nullptr,"vertex","vs_5_0",D3DCOMPILE_OPTIMIZATION_LEVEL3,0,&vertex,&errors),"compile color vertex shader");
        HRESULT result=D3DCompile(shader,sizeof(shader)-1,nullptr,nullptr,nullptr,decoder?(hdr?"decode":"decode_sdr"):(hdr?"encode":"encode_sdr"),"ps_5_0",D3DCOMPILE_OPTIMIZATION_LEVEL3,0,&pixel,&errors);
        if(FAILED(result)&&errors)throw std::runtime_error(std::string((const char*)errors->GetBufferPointer(),errors->GetBufferSize()));
        check(result,"compile color pixel shader");
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
        const char* profiling=std::getenv("UU_GPU_ENCODE_PROFILE");
        if(!decoder&&profiling&&std::strcmp(profiling,"1")==0)profile=std::make_unique<GpuProfile>(device.Get());
        if(decoder){output=texture(hdr?DXGI_FORMAT_R16G16B16A16_FLOAT:(shared>=2?DXGI_FORMAT_R8G8B8A8_UNORM:DXGI_FORMAT_B8G8R8A8_UNORM),w,h,shared);check(device->CreateRenderTargetView(output.Get(),nullptr,&target),"create decoder RGB output view");if(!hdr)prepare_source(w,h);}
    }
    ComPtr<ID3D11Texture2D> texture(DXGI_FORMAT format,unsigned w,unsigned h,unsigned exportable) {
        D3D11_TEXTURE2D_DESC td{};td.Width=w;td.Height=h;td.ArraySize=1;td.MipLevels=1;td.Format=format;td.SampleDesc.Count=1;
        td.Usage=D3D11_USAGE_DEFAULT;td.BindFlags=D3D11_BIND_SHADER_RESOURCE|D3D11_BIND_RENDER_TARGET;td.MiscFlags=exportable?(D3D11_RESOURCE_MISC_SHARED|(exportable==2?D3D11_RESOURCE_MISC_SHARED_NTHANDLE:0)):0;
        ComPtr<ID3D11Texture2D> value;check(device->CreateTexture2D(&td,nullptr,&value),"create HDR texture");return value;
    }
    std::unique_ptr<EncodeTarget> make_target() {
        if(decoder)throw std::runtime_error("decoder conversion cannot lease encoder targets");
        auto value=std::make_unique<EncodeTarget>();
        value->output=texture(hdr?DXGI_FORMAT_P010:DXGI_FORMAT_NV12,width,height,false);
        for(unsigned plane=0;plane<2;plane++) {
            D3D11_RENDER_TARGET_VIEW_DESC vd{};vd.ViewDimension=D3D11_RTV_DIMENSION_TEXTURE2D;
            vd.Format=hdr?(plane?DXGI_FORMAT_R16G16_UNORM:DXGI_FORMAT_R16_UNORM):(plane?DXGI_FORMAT_R8G8_UNORM:DXGI_FORMAT_R8_UNORM);
            check(device->CreateRenderTargetView(value->output.Get(),&vd,&value->planes[plane]),"create pooled encoder plane view");
        }
        target_allocations++;return value;
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
    void prepare_source(unsigned w,unsigned h) {
        y.Reset();uv.Reset();source=texture(hdr?DXGI_FORMAT_P010:DXGI_FORMAT_NV12,w,h,false);
        D3D11_SHADER_RESOURCE_VIEW_DESC vd{};vd.ViewDimension=D3D11_SRV_DIMENSION_TEXTURE2D;vd.Texture2D.MipLevels=1;
        vd.Format=hdr?DXGI_FORMAT_R16_UNORM:DXGI_FORMAT_R8_UNORM;
        check(device->CreateShaderResourceView(source.Get(),&vd,&y),"create decoder luma view");
        vd.Format=hdr?DXGI_FORMAT_R16G16_UNORM:DXGI_FORMAT_R8G8_UNORM;
        check(device->CreateShaderResourceView(source.Get(),&vd,&uv),"create decoder chroma view");
    }
    ComPtr<ID3D11Texture2D> convert(ID3D11Texture2D* input,unsigned index,bool full,EncodeTarget* destination=nullptr,bool matrix709=false) {
        struct Lock {ID3D10Multithread* p;Lock(ID3D10Multithread* v):p(v){p->Enter();}~Lock(){p->Leave();}} lock(threading.Get());
        const auto cpu_start=profile?std::chrono::steady_clock::now():std::chrono::steady_clock::time_point{};
        if(profile)profile->collect(context.Get());
        D3D11_TEXTURE2D_DESC td{};input->GetDesc(&td);
        ComPtr<ID3D11Device> owner;input->GetDevice(&owner);
        if(owner.Get()!=device.Get()||index>=td.ArraySize||td.MipLevels!=1||td.SampleDesc.Count!=1||td.Width<width||td.Height<height)throw std::runtime_error("GPU source device/layout mismatch");
        float values[8]={(float)width,(float)height,(float)td.Width,(float)td.Height,matrix709?1.f:0.f,hdr?1.f:0.f,full?1.f:0.f,white};
        ComPtr<ID3D11ShaderResourceView> input_view;
        if(decoder) {
            if(td.Format!=(hdr?DXGI_FORMAT_P010:DXGI_FORMAT_NV12))throw std::runtime_error("decoder plane format mismatch");
            D3D11_TEXTURE2D_DESC old{};if(source)source->GetDesc(&old);
            if(!source||old.Width!=td.Width||old.Height!=td.Height) {
                prepare_source(td.Width,td.Height);
            }
            context->CopySubresourceRegion(source.Get(),0,0,0,0,input,index,nullptr);
            ID3D11ShaderResourceView* views[]={y.Get(),uv.Get()};context->PSSetShaderResources(0,2,views);
            draw(target.Get(),width,height,values);
        } else {
            if((td.Format!=DXGI_FORMAT_R16G16B16A16_FLOAT && td.Format!=DXGI_FORMAT_B8G8R8A8_UNORM) || index!=0)throw std::runtime_error("HDR capture requires linear_rgb FP16 scRGB");
            values[6]=td.Format==DXGI_FORMAT_B8G8R8A8_UNORM?1.f:0.f;
            if(destination) {
                ComPtr<ID3D11Device> owner;destination->output->GetDevice(&owner);
                D3D11_TEXTURE2D_DESC out{};destination->output->GetDesc(&out);
                if(owner.Get()!=device.Get()||out.Width!=width||out.Height!=height||out.Format!=(hdr?DXGI_FORMAT_P010:DXGI_FORMAT_NV12))throw std::runtime_error("encoder target device/layout mismatch");
                for(auto& cached:cached_sources)if(cached.texture.Get()==input)input_view=cached.view;
                if(!input_view) {
                    check(device->CreateShaderResourceView(input,nullptr,&input_view),"create cached RGB source view");
                    cached_sources[next_source]={input,input_view};next_source=(next_source+1)%cached_sources.size();source_view_allocations++;
                }
                output=destination->output;pooled_frames++;
            } else {
                check(device->CreateShaderResourceView(input,nullptr,&input_view),"create RGB source view");
                source_view_allocations++;fresh_frames++;
                output=texture(hdr?DXGI_FORMAT_P010:DXGI_FORMAT_NV12,width,height,false);
            }
            struct EndTiming {
                GpuProfile* profile;ID3D11DeviceContext* context;GpuProfile::Slot* slot;
                ~EndTiming(){if(profile)profile->end(context,slot);}
            } timing{profile.get(),context.Get(),profile?profile->begin(context.Get()):nullptr};
            ID3D11ShaderResourceView* view=input_view.Get();context->PSSetShaderResources(0,1,&view);
            for(unsigned plane=0;plane<2;plane++) {
                D3D11_RENDER_TARGET_VIEW_DESC vd{};vd.ViewDimension=D3D11_RTV_DIMENSION_TEXTURE2D;
                vd.Format=hdr?(plane?DXGI_FORMAT_R16G16_UNORM:DXGI_FORMAT_R16_UNORM):(plane?DXGI_FORMAT_R8G8_UNORM:DXGI_FORMAT_R8_UNORM);
                ComPtr<ID3D11RenderTargetView> out;
                if(destination)out=destination->planes[plane];
                else check(device->CreateRenderTargetView(output.Get(),&vd,&out),"create encoder plane view");
                values[4]=(float)plane;draw(out.Get(),plane?width/2:width,plane?height/2:height,values);
            }
            if(profile)profile->end(context.Get(),timing.slot);
            timing.profile=nullptr;
        }
        ID3D11ShaderResourceView* none[2]={};context->PSSetShaderResources(0,2,none);context->Flush();
        if(profile)profile->cpu(std::chrono::duration<double,std::milli>(std::chrono::steady_clock::now()-cpu_start).count());
        return output;
    }
};
}
extern "C" void* eclipse_color_open(void* device,unsigned width,unsigned height,int decoder,int hdr,int shared,float white,char* error,size_t capacity) {
    try{return new Color((ID3D11Device*)device,width,height,decoder!=0,hdr!=0,shared,white);}
    catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return nullptr;}
}
extern "C" void eclipse_color_close(void* value){delete (Color*)value;}
extern "C" void* eclipse_color_target_new(void* value,char* error,size_t capacity) {
    try{return ((Color*)value)->make_target().release();}
    catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return nullptr;}
}
extern "C" void eclipse_color_target_close(void* value){delete (EncodeTarget*)value;}
extern "C" void* eclipse_color_convert_into(void* value,void* source,void* destination,char* error,size_t capacity) {
    try{auto output=((Color*)value)->convert((ID3D11Texture2D*)source,0,false,(EncodeTarget*)destination);return output.Detach();}
    catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return nullptr;}
}
extern "C" void* eclipse_color_output(void* value){return ((Color*)value)->output.Get();}
extern "C" void* eclipse_color_convert(void* value,void* source,unsigned index,int full,char* error,size_t capacity) {
    try{auto output=((Color*)value)->convert((ID3D11Texture2D*)source,index,full!=0);return output.Detach();}
    catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return nullptr;}
}

extern "C" void* eclipse_color_convert_sdr(void* value,void* source,unsigned index,int full,int matrix709,char* error,size_t capacity) {
    try{auto p=(Color*)value;if(!p->decoder||p->hdr)throw std::runtime_error("SDR decoder conversion required");auto output=p->convert((ID3D11Texture2D*)source,index,full!=0,nullptr,matrix709!=0);return output.Detach();}
    catch(const std::exception& e){if(capacity)std::snprintf(error,capacity,"%s",e.what());return nullptr;}
}
