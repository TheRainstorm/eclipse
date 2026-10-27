// Generated HDR edges -> production GPU conversion -> independent CPU math.
// No display capture or settings. MIT OR Apache-2.0.
#include <vector>
#include "../../crates/uu-media/src/d3d11_color.cpp"
namespace probe {
constexpr unsigned W=64,H=32;
using Rgb=std::array<double,3>;
double pq(double nits) {
    auto p=std::pow(std::clamp(nits/10000.,0.,1.),2610./16384.);
    return std::pow((3424./4096.+2413./128.*p)/(1.+2392./128.*p),2523./32.);
}
double eotf(double v) {
    auto p=std::pow(std::clamp(v,0.,1.),32./2523.);
    return 10000.*std::pow(std::max(p-3424./4096.,0.)/(2413./128.-2392./128.*p),16384./2610.);
}
Rgb pattern(int x,int y) {
    x=std::clamp(x,0,int(W)-1);y=std::clamp(y,0,int(H)-1);
    // Thin RGB subpixel strokes, neutral strokes, bright borders and dark glyphs.
    Rgb rgb{3.,3.,3.};
    if(x<2||y<2||x>=int(W)-2||y>=int(H)-2)rgb={12.5,12.5,12.5};
    else if((x+y/3)%11<3)rgb[(x+y)%3]=0.;
    else if((x*3+y)%17<4)rgb={0.25,0.25,0.25};
    return rgb;
}
Rgb signal(Rgb v) {
    return {pq(80.*(0.627404*v[0]+0.329283*v[1]+0.043313*v[2])),
            pq(80.*(0.069097*v[0]+0.919540*v[1]+0.011362*v[2])),
            pq(80.*(0.016391*v[0]+0.088013*v[1]+0.895595*v[2]))};
}
unsigned code(double v,unsigned lo,unsigned hi){return unsigned(std::floor(std::clamp(v,double(lo),double(hi))+0.5));}
double half(unsigned v){auto e=(v>>10)&31,m=v&1023;return (v&0x8000?-1.:1.)*(e?std::ldexp(1.+double(m)/1024.,int(e)-15):std::ldexp(double(m),-24));}
std::vector<unsigned short> words(ID3D11Device* device,ID3D11DeviceContext* context,ID3D11Texture2D* texture,unsigned rows,unsigned row_words) {
    D3D11_TEXTURE2D_DESC td{};texture->GetDesc(&td);td.Usage=D3D11_USAGE_STAGING;td.BindFlags=0;td.MiscFlags=0;td.CPUAccessFlags=D3D11_CPU_ACCESS_READ;
    ComPtr<ID3D11Texture2D> staging;check(device->CreateTexture2D(&td,nullptr,&staging),"probe staging");context->CopyResource(staging.Get(),texture);
    D3D11_MAPPED_SUBRESOURCE map{};check(context->Map(staging.Get(),0,D3D11_MAP_READ,0,&map),"probe readback");
    std::vector<unsigned short> result(rows*row_words);
    for(unsigned y=0;y<rows;y++)std::memcpy(result.data()+y*row_words,(char*)map.pData+y*map.RowPitch,row_words*2);
    context->Unmap(staging.Get(),0);return result;
}
}
int main() {
    using namespace probe;
    try {
        ComPtr<ID3D11Device> device;ComPtr<ID3D11DeviceContext> context;
        D3D_FEATURE_LEVEL level=D3D_FEATURE_LEVEL_11_0;
        check(D3D11CreateDevice(nullptr,D3D_DRIVER_TYPE_HARDWARE,nullptr,D3D11_CREATE_DEVICE_BGRA_SUPPORT,&level,1,D3D11_SDK_VERSION,&device,nullptr,&context),"probe device");
        std::vector<unsigned short> pixels(W*H*4);
        for(unsigned y=0;y<H;y++)for(unsigned x=0;x<W;x++) {
            auto p=pattern(x,y);for(unsigned c=0;c<3;c++)pixels[(y*W+x)*4+c]=p[c]==0.?0:p[c]==0.25?0x3400:p[c]==3.?0x4200:0x4a40;
            pixels[(y*W+x)*4+3]=0x3c00;
        }
        D3D11_TEXTURE2D_DESC td{};td.Width=W;td.Height=H;td.MipLevels=td.ArraySize=td.SampleDesc.Count=1;td.Format=DXGI_FORMAT_R16G16B16A16_FLOAT;td.BindFlags=D3D11_BIND_SHADER_RESOURCE;
        D3D11_SUBRESOURCE_DATA initial{pixels.data(),W*8,0};ComPtr<ID3D11Texture2D> source;
        check(device->CreateTexture2D(&td,&initial,&source),"probe FP16 input");
        Color encoder(device.Get(),W,H,false,true,false,203.);
        auto encoded=encoder.convert(source.Get(),0,false);
        auto packed=words(device.Get(),context.Get(),encoded.Get(),H*3/2,W);
        unsigned max_y=0,max_uv=0;unsigned mismatches=0;
        for(unsigned y=0;y<H;y++)for(unsigned x=0;x<W;x++) {
            auto s=signal(pattern(x,y));auto expected=code(64.+876.*(0.2627*s[0]+0.6780*s[1]+0.0593*s[2]),64,940);
            max_y=std::max(max_y,unsigned(std::abs(int(packed[y*W+x]>>6)-int(expected))));
        }
        for(unsigned y=0;y<H/2;y++)for(unsigned x=0;x<W/2;x++) {
            Rgb average{};
            for(int dy=0;dy<2;dy++)for(int dx=-1;dx<=1;dx++) {auto s=pattern(int(x*2)+dx,int(y*2)+dy);for(unsigned c=0;c<3;c++)average[c]+=s[c]*(dx==0?0.25:0.125);}
            auto s=signal(average);auto luma=0.2627*s[0]+0.6780*s[1]+0.0593*s[2];
            unsigned expected[2]={code(512.+896.*(s[2]-luma)/1.8814,64,960),code(512.+896.*(s[0]-luma)/1.4746,64,960)};
            for(unsigned c=0;c<2;c++){auto delta=unsigned(std::abs(int(packed[W*H+y*W+x*2+c]>>6)-int(expected[c])));max_uv=std::max(max_uv,delta);mismatches+=delta>2;}
        }
        Color decoder(device.Get(),W,H,true,true,false,203.);auto decoded=decoder.convert(encoded.Get(),0,false);
        auto rgb=words(device.Get(),context.Get(),decoded.Get(),H,W*4);double max_rgb=0.;
        auto sample_uv=[&](int x,int y,int c){return double(packed[W*H+std::clamp(y,0,int(H/2)-1)*W+std::clamp(x,0,int(W/2)-1)*2+c]>>6);};
        for(unsigned y=0;y<H;y++)for(unsigned x=0;x<W;x++) {
            // LEFT luma center x+0.5 corresponds to chroma position x/2;
            // vertically centered rows correspond to y/2-0.25.
            double cx=double(x)/2.,cy=double(y)/2.-0.25,tx=cx-std::floor(cx),ty=cy-std::floor(cy);double uv[2]{};
            for(int c=0;c<2;c++) {
                int ix=int(std::floor(cx)),iy=int(std::floor(cy));
                uv[c]=((sample_uv(ix,iy,c)*(1.-tx)+sample_uv(ix+1,iy,c)*tx)*(1.-ty)+(sample_uv(ix,iy+1,c)*(1.-tx)+sample_uv(ix+1,iy+1,c)*tx)*ty-512.)/896.;
            }
            double luma=(double(packed[y*W+x]>>6)-64.)/876.;
            Rgb wide{eotf(luma+1.4746*uv[1])/80.,eotf(luma-0.164553*uv[0]-0.571353*uv[1])/80.,eotf(luma+1.8814*uv[0])/80.};
            Rgb expected{1.660491*wide[0]-0.587641*wide[1]-0.072850*wide[2],-0.124550*wide[0]+1.132900*wide[1]-0.008349*wide[2],-0.018151*wide[0]-0.100579*wide[1]+1.118730*wide[2]};
            for(unsigned c=0;c<3;c++)max_rgb=std::max(max_rgb,std::abs(half(rgb[(y*W+x)*4+c])-expected[c]));
        }
        bool passed=max_y<=2&&max_uv<=2&&max_rgb<0.025;
        std::printf("{\"passed\":%s,\"luma_max_code_error\":%u,\"chroma_max_code_error\":%u,\"chroma_mismatches\":%u,\"decode_max_scrgb_error\":%.6f,\"input\":\"generated FP16 edges only\"}\n",passed?"true":"false",max_y,max_uv,mismatches,max_rgb);
        return passed?0:1;
    }catch(const std::exception& e){std::fprintf(stderr,"%s\n",e.what());return 2;}
}
