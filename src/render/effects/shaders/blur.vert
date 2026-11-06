// Draws a full-target quad from a unit square (0..1 on both axes).
// The same coordinates double as texture coordinates, so the fragment shaders read the source
// texture 1:1 across the destination.
attribute vec2 vert;
varying vec2 uv;

void main() {
    uv = vert;
    gl_Position = vec4(vert * 2.0 - 1.0, 0.0, 1.0);
}
