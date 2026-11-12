#version 100
//_DEFINES
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

// PQ needs the precision: half floats would band the signal.
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

// Final pass of an HDR output. The frame is linear BT.2020 light, 1.0 being SDR white (`sdr_white`
// cd/m²). It is placed in cd/m², its luminance is rolled off under the panel's `peak`, the colors
// the panel cannot show are desaturated into its gamut (when `map_on`), and the result is PQ coded
// in BT.2020, which is what the connector announces. This is `FinalPass::apply` in
// primitives/hdr_output.rs, keep them the same.
uniform float sdr_white;
uniform float peak;
// BT.2020 to the panel's primaries, row by row, and back.
uniform vec3 to_r;
uniform vec3 to_g;
uniform vec3 to_b;
uniform vec3 from_r;
uniform vec3 from_g;
uniform vec3 from_b;
uniform vec3 luma_2020;
uniform vec3 luma_panel;
uniform float map_on;

const float KNEE = 0.75;

// Light in cd/m² to a PQ code.
vec3 pq_encode(vec3 nits) {
    vec3 y = pow(clamp(nits / 10000.0, 0.0, 1.0), vec3(0.1593017578125));
    return pow((vec3(0.8359375) + 18.8515625 * y) / (vec3(1.0) + 18.6875 * y), vec3(78.84375));
}

// Luminance in cd/m² squeezed under `peak`: untouched up to the knee, then a smooth roll-off.
float tone_map(float nits) {
    float knee = KNEE * peak;
    if (nits <= knee)
        return nits;
    float range = peak - knee;
    return knee + range * (1.0 - exp(-(nits - knee) / range));
}

void main() {
    vec3 nits = max(texture2D(tex, v_coords).rgb, vec3(0.0)) * sdr_white;

    float y = dot(luma_2020, nits);
    if (y > 1e-6)
        nits *= tone_map(y) / y;

    if (map_on > 0.5) {
        vec3 p = vec3(dot(to_r, nits), dot(to_g, nits), dot(to_b, nits)) / peak;
        float l = clamp(dot(luma_panel, p), 0.0, 1.0);
        float low = min(p.r, min(p.g, p.b));
        float high = max(p.r, max(p.g, p.b));
        float keep = 1.0;
        if (low < 0.0)
            keep = min(keep, l / (l - low));
        if (high > 1.0)
            keep = min(keep, (1.0 - l) / (high - l));
        p = clamp(vec3(l) + (p - vec3(l)) * keep, 0.0, 1.0) * peak;
        nits = vec3(dot(from_r, p), dot(from_g, p), dot(from_b, p));
    }

    // The frame is opaque, `alpha` is 1.
    gl_FragColor = vec4(pq_encode(max(nits, vec3(0.0))), 1.0) * alpha;
}
