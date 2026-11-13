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
// in BT.2020 (`out_hlg` 0), or HLG coded for a 1000 cd/m² display (`out_hlg` 1), which is what the
// connector announces. `tone_kind` 0 rolls the luminance off with the
// knee, 1 with the BT.2390 EETF. `content_peak` above 0 is how bright the visible content says it
// gets (cd/m², `FinalPass::content_peak`): it replaces `tone_kind`, content up to the panel's peak
// is left as it is and brighter content is BT.2390-mapped with that peak as its source, 0 is
// no metadata. A link with fewer bits than the signal needs gets `dither_levels` (see `dither`). This
// is `FinalPass::apply` and `FinalPass::dither` in primitives/hdr_output.rs, keep them the same.
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
uniform float content_peak;
uniform float out_hlg;
// Highest code value of the link (255 for 8 bits), 0 for no dithering.
uniform float dither_levels;

const float KNEE = 0.75;
const float BT2390_SOURCE_PEAK = 1000.0;
const float HLG_NOMINAL_PEAK = 1000.0;
const float HLG_GAMMA = 1.2;

// Light in cd/m² to a PQ code.
vec3 pq_encode(vec3 nits) {
    vec3 y = pow(clamp(nits / 10000.0, 0.0, 1.0), vec3(0.1593017578125));
    return pow((vec3(0.8359375) + 18.8515625 * y) / (vec3(1.0) + 18.6875 * y), vec3(78.84375));
}

// The same for one value, and its inverse.
// Scene light (0 to 1) to HLG signal.
vec3 hlg_oetf(vec3 e) {
    e = clamp(e, 0.0, 1.0);
    vec3 low = sqrt(3.0 * e);
    vec3 high = 0.17883277 * log(max(12.0 * e - 0.28466892, 1e-6)) + vec3(0.55991073);
    return mix(high, low, vec3(lessThanEqual(e, vec3(1.0 / 12.0))));
}

// Light in cd/m² on a 1000 cd/m² display to the HLG signal that shows it: the inverse OOTF, then the OETF.
vec3 hlg_signal(vec3 nits) {
    vec3 rel = max(nits, vec3(0.0)) / HLG_NOMINAL_PEAK;
    float y = dot(luma_2020, rel);
    if (y <= 0.0)
        return vec3(0.0);
    return hlg_oetf(rel * pow(y, (1.0 - HLG_GAMMA) / HLG_GAMMA));
}

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

// Luminance in cd/m² squeezed under `peak` with the BT.2390 EETF for content that goes up to
// `source_peak`: untouched up to its knee, then a Hermite spline in the PQ domain that ends on the
// peak. No black lift, and light above the source peak is shown at the peak. This is `bt2390_fit`
// in primitives/hdr_output.rs.
float bt2390_fit(float nits, float source_peak) {
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

// The same with the source peak of content that says nothing, `bt2390_source_peak` in
// primitives/hdr_output.rs.
float bt2390_tone_map(float nits) {
    float source_peak = peak < BT2390_SOURCE_PEAK ? BT2390_SOURCE_PEAK : min(peak * 4.0, 10000.0);
    return bt2390_fit(nits, source_peak);
}

// Luminance in cd/m² squeezed under `peak`: untouched up to the knee, then a smooth roll-off.
float tone_map(float nits) {
    float knee = KNEE * peak;
    if (nits <= knee)
        return nits;
    float range = peak - knee;
    return knee + range * (1.0 - exp(-(nits - knee) / range));
}

// Interleaved gradient noise (Jimenez), 0 to 1, a fixed pattern per pixel. `dither_noise` in
// primitives/hdr_output.rs.
float ign(vec2 p) {
    return fract(52.9829189 * fract(dot(p, vec2(0.06711056, 0.00583715))));
}

// A code value with triangular noise of one step added before the link rounds it: a gradient
// that would band on a link with fewer bits turns into fine grain. Black is left alone, and the
// pattern is fixed to the pixel so a still picture does not shimmer.
vec3 dither(vec3 code, vec2 p) {
    vec3 a = vec3(ign(p), ign(p + vec2(41.0, 0.0)), ign(p + vec2(82.0, 0.0)));
    vec3 b = vec3(ign(p + vec2(0.0, 73.0)), ign(p + vec2(0.0, 102.0)), ign(p + vec2(0.0, 131.0)));
    vec3 lit = vec3(greaterThan(code, vec3(0.0)));
    return clamp(code + lit * (a + b - vec3(1.0)) / dither_levels, 0.0, 1.0);
}

void main() {
    vec3 nits = max(texture2D(tex, v_coords).rgb, vec3(0.0)) * sdr_white;

    float y = dot(luma_2020, nits);
    if (y > 1e-6) {
        float mapped;
        if (content_peak > 0.0)
            mapped = bt2390_fit(y, content_peak);
        else if (tone_kind > 0.5)
            mapped = bt2390_tone_map(y);
        else
            mapped = tone_map(y);
        nits *= mapped / y;
    }

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
    vec3 coded = out_hlg > 0.5 ? hlg_signal(nits) : pq_encode(max(nits, vec3(0.0)));
    if (dither_levels > 0.0)
        coded = dither(coded, gl_FragCoord.xy);
    gl_FragColor = vec4(coded, 1.0) * alpha;
}
