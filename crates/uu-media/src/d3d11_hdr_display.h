// Windows Advanced Color queries shared by capture and presentation.
#pragma once
#include <windows.h>
#include <dxgi1_5.h>
#include <vector>
#include <cwchar>
struct EclipseOutputDesc1 {WCHAR name[32];RECT rect;BOOL attached;DXGI_MODE_ROTATION rotation;HMONITOR monitor;UINT bits;DXGI_COLOR_SPACE_TYPE color;float red[2],green[2],blue[2],white[2],minimum,maximum,full;};
// Older MinGW SDKs omit Output6. GetDesc1 is its first additional method.
struct EclipseOutput6:IDXGIOutput5 {virtual HRESULT STDMETHODCALLTYPE GetDesc1(EclipseOutputDesc1*)=0;};
static const GUID eclipse_output6_iid={0x068346e8,0xaaec,0x4b84,{0xad,0xd7,0x13,0x7f,0x51,0x3f,0x77,0xa1}};
inline float eclipse_sdr_white(const WCHAR* name) {
    UINT paths_count=0,modes_count=0;
    if(GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS,&paths_count,&modes_count)!=ERROR_SUCCESS||paths_count>256||modes_count>1024)return 203.f;
    std::vector<DISPLAYCONFIG_PATH_INFO> paths(paths_count);std::vector<DISPLAYCONFIG_MODE_INFO> modes(modes_count);
    if(QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS,&paths_count,paths.data(),&modes_count,modes.data(),nullptr)!=ERROR_SUCCESS)return 203.f;
    for(UINT i=0;i<paths_count;i++) {
        DISPLAYCONFIG_SOURCE_DEVICE_NAME source{};source.header.type=DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;source.header.size=sizeof(source);source.header.adapterId=paths[i].sourceInfo.adapterId;source.header.id=paths[i].sourceInfo.id;
        if(DisplayConfigGetDeviceInfo(&source.header)!=ERROR_SUCCESS||std::wcscmp(source.viewGdiDeviceName,name)!=0)continue;
        struct White {DISPLAYCONFIG_DEVICE_INFO_HEADER header;ULONG level;} white{};
        white.header.type=(DISPLAYCONFIG_DEVICE_INFO_TYPE)11;white.header.size=sizeof(white);white.header.adapterId=paths[i].targetInfo.adapterId;white.header.id=paths[i].targetInfo.id;
        if(DisplayConfigGetDeviceInfo(&white.header)==ERROR_SUCCESS && white.level>=500 && white.level<=12500)return white.level*0.08f;
    }
    return 203.f;
}
