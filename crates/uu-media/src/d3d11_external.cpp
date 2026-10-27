// GPU timeline ownership for a shared D3D11 image. MIT OR Apache-2.0.
// The caller retains frame leases until the producer fence completes.
#include <d3d11_4.h>
#include <dxgi1_2.h>
#include <wrl/client.h>
#include <cstdio>
#include <cstdint>
#include <stdexcept>
#include <string>
using Microsoft::WRL::ComPtr;
namespace {
void check(HRESULT hr,const char* op){if(FAILED(hr))throw std::runtime_error(std::string(op)+": HRESULT="+std::to_string((unsigned long)hr));}
struct Handle {HANDLE value=nullptr;~Handle(){if(value)CloseHandle(value);}};
struct External {
    ComPtr<ID3D11Texture2D> texture;
    ComPtr<ID3D11DeviceContext4> context;
    ComPtr<ID3D11Fence> produced,consumed;
    Handle image,ready,returned;
    uint64_t write_value=0,read_value=0;
    explicit External(ID3D11Texture2D* native):texture(native) {
        ComPtr<ID3D11Device> device;texture->GetDevice(&device);
        ComPtr<ID3D11Device5> modern;check(device.As(&modern),"D3D11 external fence device");
        ComPtr<ID3D11DeviceContext> original;device->GetImmediateContext(&original);check(original.As(&context),"D3D11 external fence context");
        check(modern->CreateFence(0,D3D11_FENCE_FLAG_SHARED,IID_PPV_ARGS(&produced)),"create producer fence");
        check(modern->CreateFence(0,D3D11_FENCE_FLAG_SHARED,IID_PPV_ARGS(&consumed)),"create consumer fence");
        check(produced->CreateSharedHandle(nullptr,GENERIC_ALL,nullptr,&ready.value),"share producer fence");
        check(consumed->CreateSharedHandle(nullptr,GENERIC_ALL,nullptr,&returned.value),"share consumer fence");
        ComPtr<IDXGIResource1> resource;check(texture.As(&resource),"query external image");
        D3D11_TEXTURE2D_DESC desc{};texture->GetDesc(&desc);
        if(!(desc.MiscFlags&D3D11_RESOURCE_MISC_SHARED_NTHANDLE))throw std::runtime_error("native shared image requires an NT handle");
        check(resource->CreateSharedHandle(nullptr,DXGI_SHARED_RESOURCE_READ|DXGI_SHARED_RESOURCE_WRITE,nullptr,&image.value),"share native NT image");
    }
    void before_write(){if(read_value)check(context->Wait(consumed.Get(),read_value),"GPU wait for image reader");}
    uint64_t signal(){uint64_t value=++write_value;check(context->Signal(produced.Get(),value),"signal converted image");context->Flush();return value;}
    void idle(){
        uint64_t value=signal();Handle event;event.value=CreateEventW(nullptr,FALSE,FALSE,nullptr);
        if(!event.value)throw std::runtime_error("create GPU retirement event");
        check(produced->SetEventOnCompletion(value,event.value),"arm GPU retirement event");
        if(WaitForSingleObject(event.value,5000)!=WAIT_OBJECT_0)throw std::runtime_error("GPU retirement timed out");
    }
};
void error(char* text,size_t size,const std::exception& e){if(size)std::snprintf(text,size,"%s",e.what());}
}
extern "C" void* eclipse_external_open(void* texture,char* text,size_t size){try{return new External((ID3D11Texture2D*)texture);}catch(const std::exception& e){error(text,size,e);return nullptr;}}
extern "C" void eclipse_external_close(void* value){delete (External*)value;}
extern "C" void* eclipse_external_handle(void* value,unsigned index){auto p=(External*)value;return index==0?p->image.value:index==1?p->ready.value:p->returned.value;}
extern "C" int eclipse_external_before_write(void* value,uint64_t consumed,char* text,size_t size){try{auto p=(External*)value;p->read_value=consumed;p->before_write();return 0;}catch(const std::exception& e){error(text,size,e);return -1;}}
extern "C" uint64_t eclipse_external_ready(void* value,char* text,size_t size){try{return ((External*)value)->signal();}catch(const std::exception& e){error(text,size,e);return 0;}}
extern "C" uint64_t eclipse_external_completed(void* value){return ((External*)value)->produced->GetCompletedValue();}
extern "C" int eclipse_external_idle(void* value,char* text,size_t size){try{((External*)value)->idle();return 0;}catch(const std::exception& e){error(text,size,e);return -1;}}
