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
