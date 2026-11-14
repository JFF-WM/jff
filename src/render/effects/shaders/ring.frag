// Pixel shader (Smithay adds the version line): a rounded ring - the outline around the
// selected thumbnail in the overview.
//
// The element covers the thumbnail grown by `border` on every side. The ring is what's left of
// that rounded rectangle once the thumbnail's own rounded rectangle is cut out, so a
// translucent window doesn't show a tint behind it, and the outline follows the thumbnail's
// corners at a constant width.
//
// Window borders can also move: `animated` (0.0 to 1.0) is how much of the motion shows, and
// `style` picks it:
//   0.0  spin:  `color` blends towards `color2` by an angle-dependent weight that rotates once
//               as `phase` runs from 0.0 to 1.0.
//   1.0  sweep: the same, narrowed to a highlight that travels around the ring.
//   2.0  smoke: the ring stays as it is and smoke comes off its outer edge. The element is then
//               `margin` pixels bigger than the ring on every side, which is the room the smoke
//               has. The smoke rises, drifts and curls, and `phase` is one loop of it: the
//               picture at 1.0 is the picture at 0.0.
// With `animated` at 0.0 (the default the overview passes) the output is just `color`.
//
// A window border can also be a sheet of torn paper (`paper` at 1.0): the ring is then a piece
// of paper `border` pixels deep with a ragged edge, a little grain, lighter fibres along the
// tear, and a soft shadow in the `margin` around it. It stands still.

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
// Which animation: 0.0 spin, 1.0 sweep, 2.0 smoke.
uniform float style;
// Room around the ring the element reaches out by, in pixels. 0.0 for everything but smoke.
uniform float margin;
// 1.0: draw the ring as a sheet of torn paper, 0.0: as a plain ring.
uniform float paper;

const float TAU = 6.2831853;
// Which way up is in `v_coords`: y runs down the element, so up is minus. Flip this if a
// compositor ever hands the element's coordinates over the other way up.
const float UP = -1.0;

// Signed distance to a rounded box centred on the origin: negative inside.
float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - (half_size - vec2(r));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

// Hash of a lattice point, 0.0 to 1.0. No sin(): it loses its precision on some GPUs.
float hash(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

// Smooth value noise, 0.0 to 1.0.
float noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    vec2 u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x),
               u.y);
}

// Fractal noise: `octaves` layers of noise, each twice as fine and half as strong, and turned
// a little so the lattice's axes don't show. About 0.0 to 1.0, around 0.5.
float fbm(vec2 p, int octaves) {
    const mat2 turn = mat2(0.8, -0.6, 0.6, 0.8);
    float sum = 0.0;
    float weight = 0.5;
    float norm = 0.0;
    for (int i = 0; i < 4; i++) {
        if (i >= octaves)
            break;
        sum += weight * noise(p);
        norm += weight;
        p = turn * p * 2.03 + vec2(17.1, 9.2);
        weight *= 0.5;
    }
    return sum / norm;
}

// The smoke at `p` (pixels from the element's centre): premultiplied colour, or nothing where
// there is none. `half_size` and `r` are the ring's outer rounded box.
vec4 smoke(vec2 p, vec2 half_size, float r) {
    // The way out of the ring from here, and how far out this is. The nearest point of the box
    // the corners are rounded around gives both.
    vec2 core = max(half_size - vec2(r), vec2(0.0));
    vec2 away = p - clamp(p, -core, core);
    float len = length(away);
    float d = len - r;
    if (d <= 0.0 || margin <= 0.0)
        return vec4(0.0);
    vec2 dir = away / max(len, 0.0001);

    // Smoke rises: it gets the whole margin above the window, and a little under it, where
    // the window is in the way.
    float up = dir.y * UP;
    float reach = margin * (0.3 + 0.7 * smoothstep(-0.6, 1.0, up));
    float t = d / reach;
    if (t >= 1.0)
        return vec4(0.0);

    // Wisps are much longer than they are wide. A loop of the animation is one lap of a circle
    // for the curl, and a flow-map blend for the rise: the pattern is read at a spot, and at
    // the spot one loop of rise away, and the two are crossfaded so that the last frame of a
    // loop is the first one of the next.
    vec2 q = vec2(p.x / 34.0, p.y / 70.0);
    vec2 lap = vec2(cos(phase * TAU), sin(phase * TAU));
    vec2 curl = vec2(fbm(q * 0.8 + lap * 0.9 + vec2(3.1, 1.7), 3),
                     fbm(q * 0.8 + lap.yx * vec2(1.0, -1.0) * 0.9 + vec2(8.3, 2.8), 3)) - 0.5;
    // The curl bends the wisps sideways more the higher they are.
    q.x += curl.x * 2.6 * (0.3 + t);
    q.y += curl.y * 1.2;
    // Rising is reading further down the noise: what is at one spot now was lower before.
    float rise = -UP * 3.0;
    float now = fbm(q + vec2(0.0, rise * phase), 4);
    float next = fbm(q + vec2(0.0, rise * (phase - 1.0)), 4);
    float n = mix(now, next, phase);
    // Crossfading two pictures flattens the contrast in between: put it back.
    n = clamp(0.5 + (n - 0.5) * (1.0 + 0.7 * sin(phase * 3.14159265)), 0.0, 1.0);

    // Where the noise is high a wisp reaches far out, where it is low it stops near the ring:
    // that is what makes tongues of smoke and not an even fog. Thick at the ring, thinning
    // towards the tips.
    float reach_here = n * 1.5;
    float density = smoothstep(-0.15, 0.95, reach_here - t);
    // Soft all through, and gone well before the tips: smoke has no edge.
    density *= pow(1.0 - t, 1.3);

    // Smoke is lit from behind: thin parts glow, thick parts are dark. Hot colour at the ring,
    // the cooler one in the tips.
    vec3 hot = color2.rgb;
    vec3 cool = color.rgb;
    vec3 rgb = mix(cool, hot, (1.0 - t) * (1.0 - t));
    rgb *= 0.65 + 0.55 * (1.0 - density);

    float a = density * mix(color.a, color2.a, 0.5) * 0.7;
    return vec4(rgb * a, a);
}

// How far the torn edge of a sheet is out of true at `p`, -1.0 to 1.0: a coarse wobble and a
// fine ragged part. The sheet's size seeds it, so two windows do not tear the same way.
float tear_at(vec2 p) {
    vec2 q = p + vec2(size.x * 0.37, size.y * 0.91);
    float slow = fbm(q / 24.0, 2) - 0.5;
    float coarse = fbm(q / 9.0, 3) - 0.5;
    float fine = noise(q / 2.4) - 0.5;
    return clamp(slow * 1.6 + coarse * 1.1 + fine * 0.5, -1.0, 1.0);
}

// The sheet of paper around a window, premultiplied: `outer_half` is the sheet's rectangle
// before it is torn, `hole` how much of `p` is the window's cut-out (0.0 to 1.0), and
// `hole_half` and `hole_r` the cut-out's rounded box.
vec4 paper_sheet(vec2 p, vec2 outer_half, float hole, vec2 hole_half, float hole_r) {
    // The edge is torn in and out by up to a third of the width, at most 5 px.
    float tear = tear_at(p) * min(border * 0.35, 5.0);
    // Negative inside the sheet. How far in from the torn edge this is, in pixels.
    float e = rounded_box(p, outer_half, 0.0) - tear;
    float inside = -e;
    float cover = clamp(0.5 - e, 0.0, 1.0);

    // Paper: a fine grain, and blotches that are a shade darker or lighter.
    vec2 q = p + vec2(size.x * 0.37, size.y * 0.91);
    float grain = 0.5 * noise(q / 1.7) + 0.5 * noise(q / 0.8 + vec2(11.3, 5.7));
    float blotch = fbm(q / 40.0 + vec2(4.0, 9.0), 3);
    vec3 rgb = color.rgb * (0.94 + 0.08 * grain) * (0.95 + 0.1 * blotch);

    // A torn edge shows the fibres of the paper, which are lighter than its face, and the
    // outermost pixel is a little dark and grey where it is thin.
    float fibre = 1.0 - smoothstep(0.0, 2.8, inside);
    fibre *= 0.5 + 0.5 * noise(q / 1.3);
    rgb = mix(rgb, vec3(1.0), 0.5 * fibre);
    rgb *= 1.0 - 0.14 * (1.0 - smoothstep(0.0, 1.3, inside));

    // The window sits on the paper: it shades the paper a little around its edge.
    float to_hole = rounded_box(p, hole_half, hole_r);
    rgb *= 1.0 - 0.16 * (1.0 - smoothstep(0.0, 5.0, to_hole));

    float sheet_a = cover * (1.0 - hole) * color.a;
    vec4 sheet = vec4(rgb * sheet_a, sheet_a);

    // The shadow falls down and to the right of the sheet, and not under the window.
    float to_shadow = rounded_box(p - vec2(1.5, 3.0), outer_half, 0.0) - tear * 0.5;
    float shadow = 0.38 * (1.0 - smoothstep(-3.0, 7.0, to_shadow)) * (1.0 - hole) * color.a;
    return sheet + vec4(0.0, 0.0, 0.0, shadow) * (1.0 - sheet.a);
}

void main() {
    vec2 p = v_coords * size - 0.5 * size;

    // The ring's own rectangle: the element, less the room the smoke has around it.
    vec2 outer_half = max(0.5 * size - vec2(margin), vec2(0.0));
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
    bool smoking = animated > 0.0 && style > 1.5;
    if (animated > 0.0 && !smoking) {
        // Angle around the centre, -pi to pi. Only its cosine is used, which is the same on
        // both sides of the seam, so the colour has no jump where the angle wraps.
        float angle = atan(p.y, p.x);
        float w = 0.5 + 0.5 * cos(angle - phase * TAU);
        // Sweep narrows the bright part into a highlight.
        w = mix(w, w * w * w * w * w * w, style);
        paint = mix(color, color2, w * animated);
    }

    // Premultiplied alpha.
    vec4 result = vec4(paint.rgb, 1.0) * (paint.a * coverage * alpha);

    if (smoking) {
        // The smoke is drawn behind the ring's soft edge, and fades in and out with the focus
        // like the other animations.
        vec4 puff = smoke(p, outer_half, outer_r) * (animated * alpha);
        result += puff * (1.0 - result.a);
    }

    if (paper > 0.5)
        result = paper_sheet(p, outer_half, inner, inner_half, inner_r) * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        result = vec4(0.0, 0.2, 0.0, 0.2) + result * 0.8;
#endif

    gl_FragColor = result;
}
