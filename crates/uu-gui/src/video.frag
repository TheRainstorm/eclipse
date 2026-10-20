in vec2 uv;
out vec4 output_color;
uniform sampler2D plane0;
uniform sampler2D plane1;
uniform sampler2D plane2;
uniform int pixel_layout;
uniform vec4 coeff;
uniform vec2 levels;
void main() {
    if(pixel_layout==2) {output_color=texture(plane0,uv).bgra;return;}
    float y=(texture(plane0,uv).r-levels.x)*levels.y;
    vec2 chroma=pixel_layout==0 ? texture(plane1,uv).rg : vec2(texture(plane1,uv).r,texture(plane2,uv).r);
    chroma-=vec2(128.0/255.0);
    output_color=vec4(clamp(vec3(y+coeff.x*chroma.y,y-coeff.y*chroma.x-coeff.z*chroma.y,y+coeff.w*chroma.x),0.0,1.0),1.0);
}
