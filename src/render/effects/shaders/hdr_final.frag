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
// in BT.2020, which is what the connector announces. `tone_kind` 0 rolls the luminance off with the
// knee, 1 with the BT.2390 EETF. This is `FinalPass::apply` in primitives/hdr_output.rs, keep them
// the same.
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
uniform float tone_kind;

const float KNEE = 0.75;
const float BT2390_SOURCE_PEAK = 1000.0;

// Light in cd/m² to a PQ code.
vec3 pq_encode(vec3 nits) {
    vec3 y = pow(clamp(nits / 10000.0, 0.0, 1.0), vec3(0.1593017578125));
    return pow((vec3(0.8359375) + 18.8515625 * y) / (vec3(1.0) + 18.6875 * y), vec3(78.84375));
}

// The same for one value, and its inverse.
float pq_encode1(float nits) {
    float y = pow(clamp(nits / 10000.0, 0.0, 1.0), 0.1593017578125);
    return pow((0.8359375 + 18.8515625 * y) / (1.0 + 18.6875 * y), 78.84375);
}

float pq_decode1(float code) {
    float p = pow(clamp(code, 0.0, 1.0), 1.0 / 78.84375);
    float num = max(p - 0.8359375, 0.0);
    float den = 18.8515625 - 18.6875 * p;
    return pow(num / den, 1.0 / 0.1593017578125) * 10000.0;
}

// Luminance in cd/m² squeezed under `peak` with the BT.2390 EETF: untouched up to its knee, then a
// Hermite spline in the PQ domain that ends on the peak. No black lift, and light above the source
// peak is shown at the peak. The source peak is `bt2390_source_peak` in primitives/hdr_output.rs.
float bt2390_tone_map(float nits) {
    float source_peak = peak < BT2390_SOURCE_PEAK ? BT2390_SOURCE_PEAK : min(peak * 4.0, 10000.0);
    if (peak >= source_peak)
        return min(nits, peak);
    float source = pq_encode1(source_peak);
    float max_lum = pq_encode1(peak) / source;
    float ks = max(1.5 * max_lum - 0.5, 0.0);
    float e = pq_encode1(min(nits, source_peak)) / source;
    if (e <= ks)
        return nits;
    float t = (e - ks) / (1.0 - ks);
    float t2 = t * t;
    float t3 = t2 * t;
    float shaped = (2.0 * t3 - 3.0 * t2 + 1.0) * ks + (t3 - 2.0 * t2 + t) * (1.0 - ks)
        + (-2.0 * t3 + 3.0 * t2) * max_lum;
    return min(pq_decode1(shaped * source), peak);
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
        nits *= (tone_kind > 0.5 ? bt2390_tone_map(y) : tone_map(y)) / y;

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
