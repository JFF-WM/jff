// Downsample pass of a dual Kawase blur: the destination is half the size of the source.
//
// The destination pixel centre lands exactly on a corner shared by four source texels, so a
// bilinear sample taken there averages those four. We take four such samples, shifted by
// +-half a destination pixel on both axes (scaled by `offset`), and average them. That is a
// 4x4 source area with tent-like weights, for the price of four texture reads.
precision mediump float;

uniform sampler2D tex;
// Half of one destination pixel, in texture coordinates.
uniform vec2 half_pixel;
// Spread multiplier: 1.0 is the tightest kernel, larger values blur harder per pass.
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
