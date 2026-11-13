#version 100
//_DEFINES
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

// Pixel positions reach the thousands: half precision would blur the edge by whole pixels.
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// Window geometry in framebuffer pixels (the space of gl_FragCoord), and the corner radius in
// pixels.
uniform vec2 geo_min;
uniform vec2 geo_max;
uniform float radius;

// YUV correction (wp-color-representation): the driver turned the YUV buffer into RGB with its
// own matrix and range, this maps that RGB to what the client declared:
// rgb_out = (dot(ycc_r, rgb), dot(ycc_g, rgb), dot(ycc_b, rgb)) + ycc_off. Off when
// ycc_on is 0, and only meaningful for external (YUV) textures.
uniform vec3 ycc_r;
uniform vec3 ycc_g;
uniform vec3 ycc_b;
uniform vec3 ycc_off;
uniform float ycc_on;

// Color management (wp-color-management): the surface's own gamut and curve to the output's.
// Straight (not premultiplied) color is decoded with `cm_decode`, mapped in linear light by the
// matrix whose rows are cm_r, cm_g, cm_b, scaled by `cm_black` (gain, lift: black point
// compensation, 1 and 0 for none), brought into the output's gamut (`cm_map` 0: clipped,
// 1: desaturated toward the grey of the same luminance, `cm_luma` is the output's luminance row)
// and encoded with `cm_encode`. A curve is (kind, exponent): kind 1 is the piecewise sRGB curve,
// kind 2 PQ, kind 3 HLG, kind 4 extended linear (scRGB: not clamped, so a value above 1 or
// below 0 reaches the matrix), anything else a pure power. Off when cm_on is 0.
// For an HDR output (cm_working 1) the result is not encoded: it is linear BT.2020 light in the
// working space, 1.0 being the output's SDR white, with no gamut map. PQ (kind 2) and HLG (kind 3)
// decode to a fraction of 10000 cd/m², `cm_scale` turns that into the output's units (1.0 for the
// others). HLG weighs the colors by `cm_src_luma`, the Y row of the surface's primaries.
uniform vec3 cm_r;
uniform vec3 cm_g;
uniform vec3 cm_b;
uniform vec2 cm_decode;
uniform vec3 cm_src_luma;
uniform vec2 cm_encode;
uniform float cm_map;
uniform vec2 cm_black;
uniform vec3 cm_luma;
uniform float cm_scale;
uniform float cm_working;
uniform float cm_on;

// How much of the pixel at `p` lies inside the rounded rectangle: 1 inside, 0 outside, and a
// one-pixel soft edge in between.
float rounded_coverage(vec2 p) {
    vec2 center = 0.5 * (geo_min + geo_max);
    vec2 half_size = 0.5 * (geo_max - geo_min);
    float r = min(radius, min(half_size.x, half_size.y));
    vec2 q = abs(p - center) - (half_size - vec2(r));
    float dist = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
    return clamp(0.5 - dist, 0.0, 1.0);
}

// SMPTE ST 2084 code to light, as a fraction of 10000 cd/m².
vec3 cm_pq_to_light(vec3 v) {
    vec3 p = pow(v, vec3(1.0 / 78.84375));
    vec3 num = max(p - vec3(0.8359375), vec3(0.0));
    vec3 den = vec3(18.8515625) - 18.6875 * p;
    return pow(num / den, vec3(1.0 / 0.1593017578125));
}

// BT.2100 HLG signal to light on a 1000 cd/m² display, as a fraction of 10000 cd/m²: the inverse
// OETF, then the OOTF with a system gamma of 1.2 (the exponent 0.2 on the luminance is gamma - 1).
vec3 cm_hlg_to_light(vec3 v) {
    vec3 low = v * v / 3.0;
    vec3 high = (exp((v - vec3(0.55991073)) / 0.17883277) + vec3(0.28466892)) / 12.0;
    vec3 scene = mix(high, low, step(v, vec3(0.5)));
    float y = dot(cm_src_luma, scene);
    if (y <= 0.0)
        return vec3(0.0);
    return scene * (1000.0 / 10000.0) * pow(y, 0.2);
}

// Encoded value to linear light, for the curve described by `curve`.
vec3 cm_to_light(vec3 v, vec2 curve) {
    if (curve.x > 3.5)
        return v;
    v = clamp(v, 0.0, 1.0);
    if (curve.x > 2.5)
        return cm_hlg_to_light(v);
    if (curve.x > 1.5)
        return cm_pq_to_light(v);
    if (curve.x > 0.5) {
        vec3 low = v / 12.92;
        vec3 high = pow((v + 0.055) / 1.055, vec3(2.4));
        return mix(high, low, step(v, vec3(0.04045)));
    }
    return pow(v, vec3(curve.y));
}

// Linear light to encoded value. Light outside 0 to 1 is clipped first.
vec3 cm_from_light(vec3 l, vec2 curve) {
    l = clamp(l, 0.0, 1.0);
    if (curve.x > 0.5) {
        vec3 low = l * 12.92;
        vec3 high = 1.055 * pow(l, vec3(1.0 / 2.4)) - 0.055;
        return mix(high, low, step(l, vec3(0.0031308)));
    }
    return pow(l, vec3(1.0 / curve.y));
}

// Light in the output's primaries to light the output can show. Colors that fit are untouched;
// the others are clipped, or moved along the line to the grey of the same luminance until the
// first channel to leave the gamut is exactly on its edge.
vec3 cm_map_gamut(vec3 c) {
    if (cm_map < 0.5)
        return clamp(c, 0.0, 1.0);
    float y = clamp(dot(cm_luma, c), 0.0, 1.0);
    float low = min(c.r, min(c.g, c.b));
    float high = max(c.r, max(c.g, c.b));
    float keep = 1.0;
    if (low < 0.0)
        keep = min(keep, y / (y - low));
    if (high > 1.0)
        keep = min(keep, (1.0 - y) / (high - y));
    return clamp(vec3(y) + (c - vec3(y)) * keep, 0.0, 1.0);
}

void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif
#if defined(EXTERNAL)
    if (ycc_on > 0.5) {
        vec3 c = color.rgb;
        color.rgb = clamp(vec3(dot(ycc_r, c), dot(ycc_g, c), dot(ycc_b, c)) + ycc_off, 0.0, 1.0);
    }
#endif

    if (cm_on > 0.5) {
        // The curves are not linear, so they work on the straight color, not the premultiplied one.
        vec3 straight = color.a > 0.0 ? color.rgb / color.a : color.rgb;
        vec3 light = cm_to_light(straight, cm_decode);
        light = vec3(dot(cm_r, light), dot(cm_g, light), dot(cm_b, light)) * cm_scale;
        if (cm_working > 0.5) {
            color.rgb = max(light, 0.0) * color.a;
        } else {
            light = light * cm_black.x + vec3(cm_black.y);
            light = cm_map_gamut(light);
            color.rgb = cm_from_light(light, cm_encode) * color.a;
        }
    }

    // Only the window's own rectangle is shaped; what a client draws outside it (a shadow) is
    // left alone.
    vec2 p = gl_FragCoord.xy;
    bool inside = p.x >= geo_min.x && p.y >= geo_min.y && p.x < geo_max.x && p.y < geo_max.y;
    float coverage = inside ? rounded_coverage(p) : 1.0;

    // Premultiplied alpha: every channel scales together.
    color = color * coverage * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
