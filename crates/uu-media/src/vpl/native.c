/* MIT OR Apache-2.0. Thread-owned Intel VPL adapter against the public MIT API.
 * No runtime is linked at build time; all hardware objects remain on the
 * calling worker thread. Every buffer/driver wait has an explicit bound. */
#include <vpl/mfxvideo.h>
#include <vpl/mfxdispatcher.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#ifdef _WIN32
#include <windows.h>
#include <timeapi.h>
typedef HMODULE Module;
static Module module_open(const char *name) { return LoadLibraryExA(name, NULL, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS); }
static void *module_symbol(Module m,const char *name) {return (void *)GetProcAddress(m,name);}
static void module_close(Module m) {FreeLibrary(m);}
static uint64_t now_ms(void){return GetTickCount64();}
static void pause_ms(void){Sleep(1);}
#else
#include <dlfcn.h>
#include <time.h>
typedef void *Module;
static Module module_open(const char *name){return dlopen(name,RTLD_NOW|RTLD_LOCAL);}
static void *module_symbol(Module m,const char *name){return dlsym(m,name);}
static void module_close(Module m){dlclose(m);}
static uint64_t now_ms(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return (uint64_t)t.tv_sec*1000+t.tv_nsec/1000000;}
static void pause_ms(void){struct timespec t={0,1000000};nanosleep(&t,NULL);}
#endif

typedef mfxLoader (MFX_CDECL *LoadFn)(void);
typedef void (MFX_CDECL *UnloadFn)(mfxLoader);
typedef mfxStatus (MFX_CDECL *CreateFn)(mfxLoader,mfxU32,mfxSession *);
typedef struct {
    Module module; mfxLoader loader; UnloadFn unload;
    mfxStatus (MFX_CDECL *close)(mfxSession);
    mfxStatus (MFX_CDECL *query_impl)(mfxSession,mfxIMPL *);
    mfxStatus (MFX_CDECL *query)(mfxSession,mfxVideoParam *,mfxVideoParam *);
    mfxStatus (MFX_CDECL *init)(mfxSession,mfxVideoParam *);
    mfxStatus (MFX_CDECL *reset)(mfxSession,mfxVideoParam *);
    mfxStatus (MFX_CDECL *get)(mfxSession,mfxVideoParam *);
    mfxStatus (MFX_CDECL *encode_close)(mfxSession);
    mfxStatus (MFX_CDECL *encode)(mfxSession,mfxEncodeCtrl *,mfxFrameSurface1 *,mfxBitstream *,mfxSyncPoint *);
    mfxStatus (MFX_CDECL *sync)(mfxSession,mfxSyncPoint,mfxU32);
} Runtime;
typedef struct {
    Runtime rt; mfxSession session; int initialized; int timer;
    mfxVideoParam params;
    mfxExtCodingOption coding1; mfxExtCodingOption2 coding2; mfxExtCodingOption3 coding3;
    mfxExtVideoSignalInfo color; mfxExtAV1BitstreamParam av1;
    mfxExtBuffer *extensions[5];
    mfxFrameSurface1 surface;
    uint8_t *pixels,*bits; size_t pixel_size,bit_size;
    uint32_t frame_order,peak_kbps; int vbr;
} Encoder;
static int failed(char *error,size_t capacity,const char *operation,int status){
    if(capacity)snprintf(error,capacity,"Intel VPL %s failed (status %d)",operation,status);
    return -1;
}
static void runtime_close(Runtime *rt){
    if(rt->loader && rt->unload)rt->unload(rt->loader);
    if(rt->module)module_close(rt->module);
    memset(rt,0,sizeof(*rt));
}
static int runtime_open(Runtime *rt,char *error,size_t capacity){
#ifdef _WIN32
    const char *names[]={"libvpl.dll"};
#else
    const char *names[]={"libvpl.so.2"};
#endif
    for(size_t i=0;i<sizeof(names)/sizeof(names[0]);++i){
        rt->module=module_open(names[i]);if(!rt->module)continue;
#define RESOLVE(field,name) rt->field=(void *)module_symbol(rt->module,name)
        RESOLVE(close,"MFXClose");RESOLVE(query_impl,"MFXQueryIMPL");
        RESOLVE(query,"MFXVideoENCODE_Query");RESOLVE(init,"MFXVideoENCODE_Init");
        RESOLVE(reset,"MFXVideoENCODE_Reset");RESOLVE(get,"MFXVideoENCODE_GetVideoParam");
        RESOLVE(encode_close,"MFXVideoENCODE_Close");RESOLVE(encode,"MFXVideoENCODE_EncodeFrameAsync");
        RESOLVE(sync,"MFXVideoCORE_SyncOperation");
#undef RESOLVE
        if(rt->close&&rt->query_impl&&rt->query&&rt->init&&rt->reset&&rt->get&&rt->encode_close&&rt->encode&&rt->sync)return 0;
        runtime_close(rt);
    }
    return failed(error,capacity,"runtime unavailable; install Intel VPL dispatcher and GPU runtime",MFX_ERR_NOT_FOUND);
}
void eclipse_vpl_encoder_close(Encoder *e){
    if(!e)return;
    if(e->session){if(e->initialized)e->rt.encode_close(e->session);e->rt.close(e->session);}
    runtime_close(&e->rt);free(e->pixels);free(e->bits);
#ifdef _WIN32
    if(e->timer)timeEndPeriod(1);
#endif
    free(e);
}
static int session_open(Encoder *e,unsigned index){
    LoadFn load=(LoadFn)module_symbol(e->rt.module,"MFXLoad");
    CreateFn create=(CreateFn)module_symbol(e->rt.module,"MFXCreateSession");
    e->rt.unload=(UnloadFn)module_symbol(e->rt.module,"MFXUnload");
    if(!load||!create||!e->rt.unload)return MFX_ERR_UNSUPPORTED;
    if(!e->rt.loader)e->rt.loader=load();
    if(!e->rt.loader)return MFX_ERR_NOT_INITIALIZED;
    mfxStatus status=create(e->rt.loader,index,&e->session);
    if(status<0)return status;
    mfxIMPL implementation=0;status=e->rt.query_impl(e->session,&implementation);
    if(status<0 || !((implementation&0x0f)==MFX_IMPL_HARDWARE || (implementation&0x0f)==MFX_IMPL_HARDWARE2 || (implementation&0x0f)==MFX_IMPL_HARDWARE3 || (implementation&0x0f)==MFX_IMPL_HARDWARE4)){e->rt.close(e->session);e->session=NULL;return MFX_ERR_UNSUPPORTED;}
    return 0;
}
static void set_bitrate(Encoder *e,uint32_t target){
    uint32_t peak=e->vbr?e->peak_kbps:target;
    uint32_t multiplier=(peak+65534)/65535;if(!multiplier)multiplier=1;
    e->params.mfx.BRCParamMultiplier=(mfxU16)multiplier;
    e->params.mfx.TargetKbps=(mfxU16)((target+multiplier-1)/multiplier);
    e->params.mfx.MaxKbps=(mfxU16)((peak+multiplier-1)/multiplier);
    /* Three frames of VBV budget, no lookahead or B-frame queue. */
    uint32_t buffer=(peak*3+e->params.mfx.FrameInfo.FrameRateExtN*8-1)/(e->params.mfx.FrameInfo.FrameRateExtN*8);
    e->params.mfx.BufferSizeInKB=(mfxU16)((buffer+multiplier-1)/multiplier);
    e->params.mfx.InitialDelayInKB=0;
}
static int bitrate_matches(const Encoder *e,uint32_t target){
    uint32_t multiplier=e->params.mfx.BRCParamMultiplier?e->params.mfx.BRCParamMultiplier:1;
    uint64_t actual=(uint64_t)e->params.mfx.TargetKbps*multiplier;
    uint64_t maximum=(uint64_t)e->params.mfx.MaxKbps*multiplier;
    return actual+multiplier>=target && actual<=target+multiplier && (!e->vbr || (maximum>=actual && maximum<=e->peak_kbps+multiplier));
}
int eclipse_vpl_encoder_open(uint32_t codec,uint32_t width,uint32_t height,uint32_t fps,uint32_t target,uint32_t peak,int vbr,
    Encoder **out,char *error,size_t capacity){
    if(!out || width<2 || height<2 || width>8192 || height>8192 || (width|height)&1 || !fps || fps>240 || target<64 || target>100000 || peak<target || peak>200000)
        return failed(error,capacity,"invalid encoder settings",MFX_ERR_INVALID_VIDEO_PARAM);
    *out=NULL;Encoder *e=(Encoder *)calloc(1,sizeof(*e));if(!e)return failed(error,capacity,"allocation",MFX_ERR_MEMORY_ALLOC);
    if(runtime_open(&e->rt,error,capacity)<0){eclipse_vpl_encoder_close(e);return -1;}
    mfxU32 id=codec==0?MFX_CODEC_AVC:codec==1?MFX_CODEC_HEVC:codec==2?MFX_CODEC_AV1:0;
    if(!id){eclipse_vpl_encoder_close(e);return failed(error,capacity,"unknown codec",MFX_ERR_UNSUPPORTED);}
    mfxStatus status=MFX_ERR_NOT_FOUND,last_error=MFX_ERR_NOT_FOUND;
    for(unsigned index=0;index<16;++index){
        status=session_open(e,index);if(status<0){if(status==MFX_ERR_NOT_FOUND)break;last_error=status;continue;}
        for(unsigned tuning=0;tuning<2;++tuning){
            memset(&e->params,0,sizeof(e->params));
            e->params.AsyncDepth=1;e->params.IOPattern=MFX_IOPATTERN_IN_SYSTEM_MEMORY;
            e->params.mfx.CodecId=id;e->params.mfx.CodecProfile=codec==0?MFX_PROFILE_AVC_BASELINE:codec==1?MFX_PROFILE_HEVC_MAIN:MFX_PROFILE_AV1_MAIN;
            if(codec==0)e->params.mfx.CodecLevel=MFX_LEVEL_AVC_52;
            e->params.mfx.TargetUsage=MFX_TARGETUSAGE_BEST_SPEED;
            e->params.mfx.GopPicSize=(mfxU16)(fps*2);e->params.mfx.GopRefDist=1;
            e->params.mfx.GopOptFlag=MFX_GOP_CLOSED;e->params.mfx.NumRefFrame=1;
            e->params.mfx.RateControlMethod=vbr?MFX_RATECONTROL_VBR:MFX_RATECONTROL_CBR;
            e->params.mfx.FrameInfo.FourCC=MFX_FOURCC_NV12;
            e->params.mfx.FrameInfo.Width=(mfxU16)((width+31)&~31u);e->params.mfx.FrameInfo.Height=(mfxU16)((height+31)&~31u);
            e->params.mfx.FrameInfo.CropW=(mfxU16)width;e->params.mfx.FrameInfo.CropH=(mfxU16)height;
            e->params.mfx.FrameInfo.FrameRateExtN=fps;e->params.mfx.FrameInfo.FrameRateExtD=1;
            e->params.mfx.FrameInfo.PicStruct=MFX_PICSTRUCT_PROGRESSIVE;e->params.mfx.FrameInfo.ChromaFormat=MFX_CHROMAFORMAT_YUV420;
            e->params.mfx.FrameInfo.BitDepthLuma=8;e->params.mfx.FrameInfo.BitDepthChroma=8;
            e->vbr=vbr;e->peak_kbps=peak;set_bitrate(e,target);
            memset(&e->coding1,0,sizeof(e->coding1));e->coding1.Header.BufferId=MFX_EXTBUFF_CODING_OPTION;e->coding1.Header.BufferSz=sizeof(e->coding1);
            e->coding1.MaxDecFrameBuffering=1;e->coding1.NalHrdConformance=MFX_CODINGOPTION_OFF;
            memset(&e->coding2,0,sizeof(e->coding2));e->coding2.Header.BufferId=MFX_EXTBUFF_CODING_OPTION2;e->coding2.Header.BufferSz=sizeof(e->coding2);
            e->coding2.BRefType=MFX_B_REF_OFF;e->coding2.RepeatPPS=MFX_CODINGOPTION_ON;
            memset(&e->coding3,0,sizeof(e->coding3));e->coding3.Header.BufferId=MFX_EXTBUFF_CODING_OPTION3;e->coding3.Header.BufferSz=sizeof(e->coding3);
            e->coding3.LowDelayBRC=MFX_CODINGOPTION_ON;e->coding3.ScenarioInfo=MFX_SCENARIO_REMOTE_GAMING;
            memset(&e->color,0,sizeof(e->color));e->color.Header.BufferId=MFX_EXTBUFF_VIDEO_SIGNAL_INFO;e->color.Header.BufferSz=sizeof(e->color);
            e->color.VideoFormat=5;e->color.ColourDescriptionPresent=1;e->color.ColourPrimaries=6;e->color.TransferCharacteristics=6;e->color.MatrixCoefficients=6;
            memset(&e->av1,0,sizeof(e->av1));e->av1.Header.BufferId=MFX_EXTBUFF_AV1_BITSTREAM_PARAM;e->av1.Header.BufferSz=sizeof(e->av1);e->av1.WriteIVFHeaders=MFX_CODINGOPTION_OFF;
            unsigned count=0;
            if(codec!=2 && !tuning){e->extensions[count++]=&e->coding1.Header;e->extensions[count++]=&e->coding2.Header;}
            if(!tuning)e->extensions[count++]=&e->coding3.Header;
            e->extensions[count++]=&e->color.Header;if(codec==2)e->extensions[count++]=&e->av1.Header;
            e->params.NumExtParam=(mfxU16)count;e->params.ExtParam=e->extensions;
            mfxVideoParam checked=e->params;
            status=e->rt.query(e->session,&e->params,&checked);
            if(status<0 || status==MFX_WRN_PARTIAL_ACCELERATION){last_error=status;continue;}
            if(checked.mfx.CodecId!=id || checked.mfx.CodecProfile!=e->params.mfx.CodecProfile || checked.mfx.FrameInfo.FrameRateExtN!=fps || checked.mfx.FrameInfo.FrameRateExtD!=1 || checked.mfx.FrameInfo.CropW!=width || checked.mfx.FrameInfo.CropH!=height
                || checked.mfx.GopRefDist!=1 || checked.AsyncDepth!=1 || checked.mfx.RateControlMethod!=e->params.mfx.RateControlMethod){last_error=MFX_ERR_UNSUPPORTED;continue;}
            e->params=checked;status=e->rt.init(e->session,&e->params);
            if(status<0){last_error=status;continue;}
            if(status==MFX_WRN_PARTIAL_ACCELERATION){last_error=status;e->rt.encode_close(e->session);continue;}
            status=e->rt.get(e->session,&e->params);
            if(status<0 || e->params.mfx.CodecId!=id || e->params.mfx.FrameInfo.CropW!=width || e->params.mfx.FrameInfo.CropH!=height
                || e->params.mfx.FrameInfo.FrameRateExtN!=fps || e->params.mfx.FrameInfo.FrameRateExtD!=1
                || e->params.mfx.FrameInfo.CropX!=0 || e->params.mfx.FrameInfo.CropY!=0 || e->params.mfx.CodecProfile!=checked.mfx.CodecProfile
                || e->params.mfx.GopRefDist!=1 || e->params.AsyncDepth!=1 || e->params.mfx.FrameInfo.FourCC!=MFX_FOURCC_NV12
                || e->params.IOPattern!=MFX_IOPATTERN_IN_SYSTEM_MEMORY || e->params.mfx.RateControlMethod!=(vbr?MFX_RATECONTROL_VBR:MFX_RATECONTROL_CBR) || !bitrate_matches(e,target)){
                last_error=status<0?status:MFX_ERR_UNSUPPORTED;e->rt.encode_close(e->session);continue;
            }
            e->initialized=1;break;
        }
        if(e->initialized)break;
        e->rt.close(e->session);e->session=NULL;
    }
    if(!e->initialized){eclipse_vpl_encoder_close(e);return failed(error,capacity,"hardware codec/rate-control initialization",last_error);}
    e->surface.Info=e->params.mfx.FrameInfo;
    uint32_t pitch=e->surface.Info.Width,rows=e->surface.Info.Height;
    e->pixel_size=(size_t)pitch*rows*3/2;e->pixels=(uint8_t *)malloc(e->pixel_size);
    e->bit_size=(size_t)width*height*3/2+65536;if(e->bit_size>64*1024*1024)e->bit_size=64*1024*1024;
    e->bits=(uint8_t *)malloc(e->bit_size);
    if(!e->pixels||!e->bits){eclipse_vpl_encoder_close(e);return failed(error,capacity,"frame allocation",MFX_ERR_MEMORY_ALLOC);}
    memset(e->pixels,16,(size_t)pitch*rows);memset(e->pixels+(size_t)pitch*rows,128,(size_t)pitch*rows/2);
    e->surface.Data.Pitch=(mfxU16)pitch;e->surface.Data.Y=e->pixels;e->surface.Data.U=e->pixels+(size_t)pitch*rows;e->surface.Data.V=e->surface.Data.U+1;
#ifdef _WIN32
    e->timer=timeBeginPeriod(1)==0;
#endif
    *out=e;return 0;
}
int eclipse_vpl_encoder_input(Encoder *e,uint8_t **data,size_t *length,uint32_t *pitch,uint32_t *rows){
    if(!e || e->surface.Data.Locked)return -1;
    *data=e->pixels;*length=e->pixel_size;*pitch=e->surface.Data.Pitch;*rows=e->surface.Info.Height;return 0;
}
int eclipse_vpl_encoder_encode(Encoder *e,uint64_t timestamp,int keyframe,const uint8_t **data,size_t *length,uint64_t *pts,int *key,char *error,size_t capacity){
    if(!e || e->initialized!=1)return failed(error,capacity,"missing/failed encoder",MFX_ERR_NOT_INITIALIZED);
    *data=NULL;*length=0;
    mfxBitstream bits;memset(&bits,0,sizeof(bits));bits.Data=e->bits;bits.MaxLength=(mfxU32)e->bit_size;
    e->surface.Data.TimeStamp=timestamp;e->surface.Data.FrameOrder=e->frame_order++;
    mfxEncodeCtrl control;memset(&control,0,sizeof(control));if(keyframe)control.FrameType=MFX_FRAMETYPE_I|MFX_FRAMETYPE_IDR|MFX_FRAMETYPE_REF;
    mfxSyncPoint sync=NULL;mfxStatus status;uint64_t deadline=now_ms()+1000;
    do{status=e->rt.encode(e->session,&control,&e->surface,&bits,&sync);if(status==MFX_WRN_DEVICE_BUSY)pause_ms();}
        while(status==MFX_WRN_DEVICE_BUSY && now_ms()<deadline);
    if(status==MFX_ERR_MORE_DATA)return 0;
    if(status<0 || !sync)return failed(error,capacity,"EncodeFrameAsync",status);
    do{status=e->rt.sync(e->session,sync,100);}while(status==MFX_WRN_IN_EXECUTION && now_ms()<deadline);
    if(status!=MFX_ERR_NONE)return failed(error,capacity,"encode synchronization",status);
    if(!bits.DataLength || (size_t)bits.DataOffset+bits.DataLength>e->bit_size)return failed(error,capacity,"invalid coded buffer",MFX_ERR_NOT_ENOUGH_BUFFER);
    *data=e->bits+bits.DataOffset;*length=bits.DataLength;*pts=bits.TimeStamp;*key=(bits.FrameType&(MFX_FRAMETYPE_I|MFX_FRAMETYPE_IDR))!=0;return 1;
}
int eclipse_vpl_encoder_bitrate(Encoder *e,uint32_t target,char *error,size_t capacity){
    if(!e || target<64 || target>100000 || (e->vbr && target>e->peak_kbps))return failed(error,capacity,"invalid bitrate",MFX_ERR_INVALID_VIDEO_PARAM);
    mfxVideoParam previous=e->params;set_bitrate(e,target);
    mfxStatus status=e->rt.reset(e->session,&e->params);
    if(status<0 || status==MFX_WRN_PARTIAL_ACCELERATION){e->params=previous;e->initialized=-1;return failed(error,capacity,"bitrate reset",status);}
    status=e->rt.get(e->session,&e->params);
    if(status<0 || !bitrate_matches(e,target) || e->params.AsyncDepth!=1 || e->params.mfx.GopRefDist!=1
        || e->params.mfx.FrameInfo.CropW!=previous.mfx.FrameInfo.CropW || e->params.mfx.FrameInfo.CropH!=previous.mfx.FrameInfo.CropH
        || e->params.mfx.FrameInfo.FrameRateExtN!=previous.mfx.FrameInfo.FrameRateExtN || e->params.mfx.FrameInfo.FrameRateExtD!=previous.mfx.FrameInfo.FrameRateExtD
        || e->params.mfx.CodecId!=previous.mfx.CodecId || e->params.mfx.RateControlMethod!=previous.mfx.RateControlMethod){
        e->initialized=-1;return failed(error,capacity,"changed configuration after bitrate reset",status<0?status:MFX_ERR_UNSUPPORTED);
    }
    return 0;
}
