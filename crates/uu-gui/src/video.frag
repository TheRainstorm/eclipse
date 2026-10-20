in vec2 uv;
out vec4 output_color;
uniform sampler2D plane0;
uniform sampler2D plane1;
uniform sampler2D plane2;
uniform int pixel_layout;
uniform vec4 coeff;
uniform vec2 levels;
uniform vec4 video_uv_layout;
void main() {
    vec2 video_uv=uv*video_uv_layout.xy;
    vec2 luma_uv=clamp(video_uv,video_uv_layout.zw*0.5,video_uv_layout.xy-video_uv_layout.zw*0.5);
    vec2 chroma_uv=clamp(video_uv,video_uv_layout.zw,video_uv_layout.xy-video_uv_layout.zw);
    if(pixel_layout==2) {output_color=vec4(texture(plane0,luma_uv).bgr,1.0);return;}
    float y=(texture(plane0,luma_uv).r-levels.x)*levels.y;
    vec2 chroma=pixel_layout==0 ? texture(plane1,chroma_uv).rg : vec2(texture(plane1,chroma_uv).r,texture(plane2,chroma_uv).r);
    chroma-=vec2(128.0/255.0);
    output_color=vec4(clamp(vec3(y+coeff.x*chroma.y,y-coeff.y*chroma.x-coeff.z*chroma.y,y+coeff.w*chroma.x),0.0,1.0),1.0);
}
