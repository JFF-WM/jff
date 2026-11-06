// Upsample pass of a dual Kawase blur: the destination is twice the size of the source.
//
// A bilinear read is already a small tent filter. Averaging four of them, shifted diagonally
// by +-half a *source* pixel (scaled by `offset`), widens that tent and hides the blocky
// pattern that plain bilinear upscaling would leave.
precision mediump float;

uniform sampler2D tex;
// Half of one source pixel, in texture coordinates.
uniform vec2 half_pixel;
uniform float offset;

varying vec2 uv;

void main() {
    vec2 d = half_pixel * offset;
    vec4 sum = texture2D(tex, uv + vec2(-d.x, -d.y))
             + texture2D(tex, uv + vec2( d.x, -d.y))
             + texture2D(tex, uv + vec2(-d.x,  d.y))
             + texture2D(tex, uv + vec2( d.x,  d.y));
    gl_FragColor = sum * 0.25;
}
