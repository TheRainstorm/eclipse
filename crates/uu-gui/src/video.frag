in vec2 uv;
out vec4 output_color;
uniform sampler2D plane0;
uniform sampler2D plane1;
uniform sampler2D plane2;
uniform int pixel_layout;
uniform bool hdr_input;
uniform bool hdr_output;
uniform bool linear_output;
uniform float sdr_white;
uniform float source_white;
float gamma_channel(float v){float a=abs(v);return sign(v)*(a<=0.0031308?a*12.92:1.055*pow(a,1.0/2.4)-0.055);}
vec3 hdr_color(vec3 rgb){
    if(hdr_output) return rgb;
    rgb=max(rgb*80.0/source_white,vec3(0));float peak=max(rgb.r,max(rgb.g,rgb.b));
    if(peak>0.75) rgb=(rgb/peak)*(1.0-0.25*exp(-4.0*(peak-0.75)));
    return vec3(gamma_channel(rgb.r),gamma_channel(rgb.g),gamma_channel(rgb.b));
}
float linear_channel(float v){float a=abs(v);return sign(v)*(a<=0.04045?a/12.92:pow((a+0.055)/1.055,2.4));}
vec3 cursor_linear(vec3 rgb){return vec3(linear_channel(rgb.r),linear_channel(rgb.g),linear_channel(rgb.b))*sdr_white;}
vec3 cursor_gamma(vec3 rgb){rgb/=sdr_white;return vec3(gamma_channel(rgb.r),gamma_channel(rgb.g),gamma_channel(rgb.b));}
uniform vec4 coeff;
uniform vec4 levels;
uniform vec4 video_uv_layout;
uniform sampler2D cursor_alpha;
uniform sampler2D cursor_xor;
uniform bool cursor_enabled;
uniform vec4 cursor_bounds;
void video_main() {
    vec2 video_uv=uv*video_uv_layout.xy;
    vec2 luma_uv=clamp(video_uv,video_uv_layout.zw*0.5,video_uv_layout.xy-video_uv_layout.zw*0.5);
    vec2 chroma_uv=clamp(video_uv+vec2(pixel_layout==5?video_uv_layout.z*0.5:0.0,0.0),video_uv_layout.zw,video_uv_layout.xy-video_uv_layout.zw);
    if(pixel_layout==3 || pixel_layout==4) {vec3 rgb=texture(plane0,luma_uv).rgb;output_color=vec4(hdr_input?hdr_color(rgb):(hdr_output?cursor_linear(rgb):rgb),1.0);return;}
    if(pixel_layout==2) {vec3 rgb=texture(plane0,luma_uv).bgr;output_color=vec4(hdr_output?cursor_linear(rgb):rgb,1.0);return;}
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
    } else {rgb=clamp(rgb,0.0,1.0);output_color=vec4(hdr_output?cursor_linear(rgb):rgb,1.0);}
}
void main() {
    video_main();
    if(linear_output && !hdr_output) output_color.rgb=cursor_linear(output_color.rgb);
    if(cursor_enabled) {
        vec2 p=(uv-cursor_bounds.xy)/cursor_bounds.zw;
        if(all(greaterThanEqual(p,vec2(0))) && all(lessThan(p,vec2(1)))) {
            vec4 cursor=texture(cursor_alpha,p);
            vec3 rgb=mix(output_color.rgb,linear_output?cursor_linear(cursor.rgb):cursor.rgb,cursor.a);
            ivec3 operand=ivec3(round(texture(cursor_xor,p).rgb*255.0));
            if(any(notEqual(operand,ivec3(0)))) {
                vec3 gamma_rgb=linear_output?cursor_gamma(rgb):rgb;
                gamma_rgb=vec3(ivec3(round(clamp(gamma_rgb,0.0,1.0)*255.0))^operand)/255.0;
                rgb=linear_output?cursor_linear(gamma_rgb):gamma_rgb;
            }
            output_color=vec4(rgb,1.0);
        }
    }
}
