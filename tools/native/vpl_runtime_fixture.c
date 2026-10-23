/* Isolated ABI contract test double. Never install this as an actual runtime. */
#include <vpl/mfxvideo.h>
#include <vpl/mfxdispatcher.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
typedef struct {mfxVideoParam params;int initialized;} Session;
static int sessions=0,bad_close=0,loaders=0;
__attribute__((destructor)) static void verify_cleanup(void){if(sessions || bad_close || loaders)_Exit(99);}
static int mode(const char *value){const char *which=getenv("ECLIPSE_VPL_CONTRACT_CASE");return which && strcmp(which,value)==0;}
mfxLoader MFXLoad(void){++loaders;return (mfxLoader)1;}
void MFXUnload(mfxLoader loader){(void)loader;--loaders;}
mfxConfig MFXCreateConfig(mfxLoader loader){(void)loader;return (mfxConfig)2;}
mfxStatus MFXSetConfigFilterProperty(mfxConfig config,const mfxU8 *name,mfxVariant value){(void)config;(void)name;return value.Type==MFX_VARIANT_TYPE_U32 && value.Version.Version==MFX_VARIANT_VERSION?0:MFX_ERR_INVALID_VIDEO_PARAM;}
mfxStatus MFXCreateSession(mfxLoader loader,mfxU32 index,mfxSession *out){(void)loader;if(index)return MFX_ERR_NOT_FOUND;*out=(mfxSession)calloc(1,sizeof(Session));if(*out)++sessions;return *out?0:MFX_ERR_MEMORY_ALLOC;}
mfxStatus MFXClose(mfxSession session){if(((Session *)session)->initialized)++bad_close;free(session);--sessions;return 0;}
mfxStatus MFXQueryIMPL(mfxSession session,mfxIMPL *implementation){(void)session;*implementation=(mode("software")||mode("decode_software"))?MFX_IMPL_SOFTWARE:MFX_IMPL_HARDWARE;return 0;}
mfxStatus MFXVideoENCODE_Query(mfxSession session,mfxVideoParam *input,mfxVideoParam *output){
    (void)session;
    if(mode("low_power_unavailable") && input->mfx.LowPower==MFX_CODINGOPTION_ON)return MFX_ERR_UNSUPPORTED;
    if(input->AsyncDepth!=1 || input->mfx.GopRefDist!=1 || input->IOPattern!=MFX_IOPATTERN_IN_SYSTEM_MEMORY || input->mfx.NumRefFrame!=1 || input->mfx.FrameInfo.FourCC!=MFX_FOURCC_NV12)return MFX_ERR_INVALID_VIDEO_PARAM;
    *output=*input;if(mode("fps"))output->mfx.FrameInfo.FrameRateExtN/=2;if(mode("mode"))output->mfx.RateControlMethod=MFX_RATECONTROL_CBR;return 0;
}
mfxStatus MFXVideoENCODE_Init(mfxSession session,mfxVideoParam *params){((Session *)session)->params=*params;((Session *)session)->initialized=1;if(mode("init_fps"))((Session *)session)->params.mfx.FrameInfo.FrameRateExtN/=2;if(mode("codec"))((Session *)session)->params.mfx.CodecId=MFX_CODEC_AVC;if(mode("low_power_changed"))((Session *)session)->params.mfx.LowPower=MFX_CODINGOPTION_OFF;return mode("partial")?MFX_WRN_PARTIAL_ACCELERATION:0;}
mfxStatus MFXVideoENCODE_GetVideoParam(mfxSession session,mfxVideoParam *params){*params=((Session *)session)->params;return 0;}
mfxStatus MFXVideoENCODE_Reset(mfxSession session,mfxVideoParam *params){
    Session *state=(Session *)session;
    /* Rate changes cannot silently resize the VBV allocated by Init. */
    mfxU32 before=state->params.mfx.BRCParamMultiplier?state->params.mfx.BRCParamMultiplier:1;
    mfxU32 after=params->mfx.BRCParamMultiplier?params->mfx.BRCParamMultiplier:1;
    if((mfxU32)params->mfx.BufferSizeInKB*after!=(mfxU32)state->params.mfx.BufferSizeInKB*before
        || (mfxU32)params->mfx.InitialDelayInKB*after!=(mfxU32)state->params.mfx.InitialDelayInKB*before)return MFX_ERR_INCOMPATIBLE_VIDEO_PARAM;
    state->params=*params;return 0;
}
mfxStatus MFXVideoENCODE_Close(mfxSession session){((Session *)session)->initialized=0;return 0;}
mfxStatus MFXVideoCORE_SyncOperation(mfxSession session,mfxSyncPoint point,mfxU32 wait){(void)session;(void)point;(void)wait;return 0;}
mfxStatus MFXVideoENCODE_EncodeFrameAsync(mfxSession session,mfxEncodeCtrl *control,mfxFrameSurface1 *surface,mfxBitstream *bits,mfxSyncPoint *sync){
    Session *state=(Session *)session;
    if(!state->initialized || !surface || (!mode("native_av1") && (surface->Data.Y[0]!=126 || surface->Data.U[0]!=128 || surface->Data.V[0]!=128 || !(control->FrameType&MFX_FRAMETYPE_IDR))))return MFX_ERR_INVALID_VIDEO_PARAM;
    /* Deliberately minimal syntax: checks the ABI/transport only, not decoding. */
    const mfxU8 hevc[]={0,0,0,1,64,1,7,0,0,0,1,66,1,8,0,0,0,1,68,1,9,0,0,0,1,38,1,10};
    const mfxU8 av1[]={18,0,10,13,0,0,0,3,55,255,230,215,200,128,128,128,130,50,22,16,0,132,0,2,8,32,136,0,0,193,180,208,193,77,245,16,137,79,197,162,188};
    const mfxU8 *data=state->params.mfx.CodecId==MFX_CODEC_AV1?av1:hevc;
    size_t size=state->params.mfx.CodecId==MFX_CODEC_AV1?sizeof(av1):sizeof(hevc);
    if(bits->MaxLength<size)return MFX_ERR_NOT_ENOUGH_BUFFER;
    memcpy(bits->Data,data,size);bits->DataOffset=mode("buffer")?bits->MaxLength+1:0;bits->DataLength=(mfxU32)size;
    bits->TimeStamp=mode("timestamp")?123456789:surface->Data.TimeStamp;bits->FrameType=MFX_FRAMETYPE_I|MFX_FRAMETYPE_IDR|MFX_FRAMETYPE_REF;*sync=(mfxSyncPoint)1;return 0;
}

mfxStatus MFXVideoDECODE_DecodeHeader(mfxSession session,mfxBitstream *bits,mfxVideoParam *params){
    (void)session;(void)bits;params->mfx.CodecId=MFX_CODEC_AV1;params->mfx.CodecProfile=MFX_PROFILE_AV1_MAIN;
    mfxFrameInfo *info=&params->mfx.FrameInfo;info->FourCC=MFX_FOURCC_NV12;info->Width=128;info->Height=128;
    info->CropW=mode("decode_size")?127:128;info->CropH=128;info->BitDepthLuma=8;info->BitDepthChroma=8;
    info->ChromaFormat=MFX_CHROMAFORMAT_YUV420;return 0;
}
mfxStatus MFXVideoDECODE_Query(mfxSession session,mfxVideoParam *input,mfxVideoParam *out){(void)session;*out=*input;return mode("decode_partial")?MFX_WRN_PARTIAL_ACCELERATION:0;}
mfxStatus MFXVideoDECODE_QueryIOSurf(mfxSession session,mfxVideoParam *params,mfxFrameAllocRequest *request){(void)session;request->Info=params->mfx.FrameInfo;if(mode("decode_alignment"))request->Info.Height+=16;if(mode("decode_allocation"))request->Info.Height=64;request->NumFrameMin=2;request->NumFrameSuggested=mode("decode_pool")?33:2;return 0;}
mfxStatus MFXVideoDECODE_Init(mfxSession session,mfxVideoParam *params){((Session *)session)->params=*params;((Session *)session)->initialized=1;return 0;}
mfxStatus MFXVideoDECODE_GetVideoParam(mfxSession session,mfxVideoParam *params){mfxStatus status=MFXVideoENCODE_GetVideoParam(session,params);if(mode("decode_alignment")){params->mfx.FrameInfo.Height+=16;params->mfx.FrameInfo.CropH+=16;}return status;}
mfxStatus MFXVideoDECODE_Reset(mfxSession session,mfxVideoParam *params){return MFXVideoENCODE_Reset(session,params);}
mfxStatus MFXVideoDECODE_Close(mfxSession session){return MFXVideoENCODE_Close(session);}
mfxStatus MFXVideoDECODE_DecodeFrameAsync(mfxSession session,mfxBitstream *bits,mfxFrameSurface1 *work,mfxFrameSurface1 **out,mfxSyncPoint *sync){
    (void)session;memset(work->Data.Y,126+(bits->TimeStamp%3),(size_t)work->Data.Pitch*work->Info.Height);memset(work->Data.U,128,(size_t)work->Data.Pitch*work->Info.Height/2);
    if(mode("decode_output_crop"))work->Info.CropH+=16;
    work->Data.TimeStamp=mode("decode_timestamp")?123456789:bits->TimeStamp;
    bits->DataOffset+=bits->DataLength;bits->DataLength=0;
    *out=mode("decode_surface")?(mfxFrameSurface1 *)(uintptr_t)0x123450:work;*sync=(mfxSyncPoint)1;return 0;
}
