in vec2 uv;
out vec4 output_color;
uniform sampler2D plane0;
uniform sampler2D plane1;
uniform sampler2D plane2;
uniform int pixel_layout;
uniform bool hdr_input;
uniform bool hdr_output;
uniform float sdr_white;
uniform float source_white;
float gamma_channel(float v){float a=abs(v);return sign(v)*(a<=0.0031308?a*12.92:1.055*pow(a,1.0/2.4)-0.055);}
vec3 hdr_color(vec3 rgb){
    if(hdr_output) rgb/=sdr_white;
    else {
        rgb=max(rgb*80.0/source_white,vec3(0));float peak=max(rgb.r,max(rgb.g,rgb.b));
        if(peak>0.75) rgb=(rgb/peak)*(1.0-0.25*exp(-4.0*(peak-0.75)));
    }
    return vec3(gamma_channel(rgb.r),gamma_channel(rgb.g),gamma_channel(rgb.b));
}
uniform vec4 coeff;
uniform vec4 levels;
uniform vec4 video_uv_layout;
void main() {
    vec2 video_uv=uv*video_uv_layout.xy;
    vec2 luma_uv=clamp(video_uv,video_uv_layout.zw*0.5,video_uv_layout.xy-video_uv_layout.zw*0.5);
    vec2 chroma_uv=clamp(video_uv,video_uv_layout.zw,video_uv_layout.xy-video_uv_layout.zw);
    if(pixel_layout==3 || pixel_layout==4) {vec3 rgb=texture(plane0,luma_uv).rgb;output_color=vec4(hdr_input?hdr_color(rgb):rgb,1.0);return;}
    if(pixel_layout==2) {output_color=vec4(texture(plane0,luma_uv).bgr,1.0);return;}
    float factor=pixel_layout==5 ? 65535.0/(64.0*1023.0) : 1.0;
    float y=(texture(plane0,luma_uv).r*factor-levels.x)*levels.y;
    vec2 chroma=(pixel_layout==0 || pixel_layout==5) ? texture(plane1,chroma_uv).rg : vec2(texture(plane1,chroma_uv).r,texture(plane2,chroma_uv).r);
    chroma=(chroma*factor-vec2(levels.z))*levels.w;
    vec3 rgb=vec3(y+coeff.x*chroma.y,y-coeff.y*chroma.x-coeff.z*chroma.y,y+coeff.w*chroma.x);
    if(hdr_input) {
        vec3 p=pow(clamp(rgb,0.0,1.0),vec3(32.0/2523.0));
        vec3 wide=10000.0/80.0*pow(max(p-3424.0/4096.0,vec3(0))/(2413.0/128.0-(2392.0/128.0)*p),vec3(16384.0/2610.0));
        rgb=vec3(dot(wide,vec3(1.660491,-0.587641,-0.072850)),dot(wide,vec3(-0.124550,1.132900,-0.008349)),dot(wide,vec3(-0.018151,-0.100579,1.118730)));
        output_color=vec4(hdr_color(rgb),1.0);
    } else {output_color=vec4(clamp(rgb,0.0,1.0),1.0);}
}
