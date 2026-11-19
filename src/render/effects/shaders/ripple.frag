// Pixel shader (Smithay adds the version line): the ripples that spread from the top-left
// corner of a monitor when the overview opens, the way GNOME Shell shows its hot corner going
// off.
//
// The element is a square with the corner at its own top-left. Three discs grow from the corner
// and fade out, each with a soft glow that is brightest at its rim, one after another: a quick
// first one, and two later ones that reach a little less far.

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

uniform float alpha;
uniform vec2 size;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// Milliseconds since the overview opened.
uniform float time;
// Radius of a ripple at scale 1.0, in pixels.
uniform float base;
// Straight (not premultiplied) colour.
uniform vec4 color;

// Fast start, gentle stop.
float ease_out_quad(float t) {
    return 1.0 - (1.0 - t) * (1.0 - t);
}

// How much of the ripple that starts `delay` ms in and lasts `duration` ms covers a point `d`
// pixels from the corner, 0.0 to 1.0. It grows from `from` to `to` times `base` and `level` is
// how bright it starts, fading to nothing as it ends.
float ripple(float d, float delay, float duration, float from, float to, float level) {
    float t = (time - delay) / duration;
    if (t <= 0.0 || t >= 1.0)
        return 0.0;
    float e = ease_out_quad(t);
    float r = max(base * mix(from, to, e), 1.0);

    // Dim in the middle, bright at the rim, with an antialiased edge.
    float inside = 1.0 - smoothstep(r - 1.0, r + 1.0, d);
    float fill = clamp(d / r, 0.0, 1.0);
    float glow = inside * fill * fill;
    float rim_width = 1.0 + 0.04 * r;
    float off = (d - r) / rim_width;
    float rim = exp(-off * off);
    return level * (1.0 - e) * clamp(0.45 * glow + 0.55 * rim, 0.0, 1.0);
}

void main() {
    float d = length(v_coords * size);

    float a = ripple(d, 0.0, 830.0, 0.25, 1.5, 1.0)
            + ripple(d, 50.0, 1000.0, 0.0, 1.25, 0.8)
            + ripple(d, 350.0, 1000.0, 0.0, 1.25, 0.8);

    // Premultiplied alpha.
    vec4 result = vec4(color.rgb, 1.0) * (color.a * clamp(a, 0.0, 1.0) * alpha);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        result = vec4(0.0, 0.2, 0.0, 0.2) + result * 0.8;
#endif

    gl_FragColor = result;
}
