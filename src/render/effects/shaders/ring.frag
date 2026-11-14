// Pixel shader (Smithay adds the version line): a rounded ring - the outline around the
// selected thumbnail in the overview.
//
// The element covers the thumbnail grown by `border` on every side. The ring is what's left of
// that rounded rectangle once the thumbnail's own rounded rectangle is cut out, so a
// translucent window doesn't show a tint behind it, and the outline follows the thumbnail's
// corners at a constant width.
//
// Window borders can also turn: `animated` (0.0 to 1.0) blends `color` towards `color2` by an
// angle-dependent weight that rotates once as `phase` runs from 0.0 to 1.0. With `animated`
// at 0.0 (the default the overview passes) the output is just `color`.

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

// Ring width, in pixels.
uniform float border;
// Corner radius of the thumbnail inside the ring, in pixels. The ring's outer corners are this
// plus `border`, so both curves share a centre.
uniform float radius;
// Straight (not premultiplied) colour.
uniform vec4 color;
// Second colour of the animation, straight RGBA.
uniform vec4 color2;
// Position of the animation in its turn, 0.0 to 1.0. Kept small on purpose: the caller wraps
// it, so float precision never degrades with uptime.
uniform float phase;
// How much of the animation shows: 0.0 none, 1.0 all. Scaled by the focus fade.
uniform float animated;
// 0.0: a gradient turning around the ring, 1.0: a narrow highlight travelling around it.
uniform float sweep;

// Signed distance to a rounded box centred on the origin: negative inside.
float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - (half_size - vec2(r));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec2 p = v_coords * size - 0.5 * size;

    vec2 outer_half = 0.5 * size;
    // Radius 0 keeps a square thumbnail's outline square.
    float outer_r = radius > 0.0 ? min(radius + border, min(outer_half.x, outer_half.y)) : 0.0;
    float outer = clamp(0.5 - rounded_box(p, outer_half, outer_r), 0.0, 1.0);

    // The cut-out reaches one pixel under the thumbnail, which is drawn on top of it, so the
    // soft edges of the two never leave a hairline gap.
    vec2 inner_half = max(outer_half - vec2(border) - vec2(1.0), vec2(0.0));
    float inner_r = max(min(radius - 1.0, min(inner_half.x, inner_half.y)), 0.0);
    float inner = clamp(0.5 - rounded_box(p, inner_half, inner_r), 0.0, 1.0);

    float coverage = outer * (1.0 - inner);

    vec4 paint = color;
    if (animated > 0.0) {
        // Angle around the centre, -pi to pi. Only its cosine is used, which is the same on
        // both sides of the seam, so the colour has no jump where the angle wraps.
        float angle = atan(p.y, p.x);
        float w = 0.5 + 0.5 * cos(angle - phase * 6.2831853);
        // Sweep narrows the bright part into a highlight.
        w = mix(w, w * w * w * w * w * w, sweep);
        paint = mix(color, color2, w * animated);
    }

    // Premultiplied alpha.
    vec4 result = vec4(paint.rgb, 1.0) * (paint.a * coverage * alpha);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        result = vec4(0.0, 0.2, 0.0, 0.2) + result * 0.8;
#endif

    gl_FragColor = result;
}
