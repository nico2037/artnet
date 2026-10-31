in vec2 v_uv;               // aperture coords, unit circle = beam edge
flat in vec3 v_wheel1;
flat in vec3 v_wheel2;
flat in vec2 v_range1;
flat in vec2 v_range2;

uniform sampler2DArray u_gobos;
uniform float u_wheel_slots;  // open slot + one per gobo layer
uniform float u_lod_base;     // gobo texture -> tile minification

out vec4 o_light;

const float SLOT_PITCH = 2.3;  // slot spacing in aperture radii; > 2 leaves metal between slots

mat2 rot2(float a) {
    float c = cos(a), s = sin(a);
    return mat2(c, s, -s, c);
}

// Slots never overlap, so even mid-scroll this is a single fetch.
// range.y == 0: demo wheel = open slot + every library layer. Otherwise a GDTF wheel of range.y
// slots stored from layer range.x on; wheel.x is the 0-based slot position.
float goboWheel(vec2 uv, vec3 wheel, vec2 range) {
    bool gdtf = range.y > 0.5;
    float slots = gdtf ? range.y : u_wheel_slots;
    float pos = mod(wheel.x, slots);
    float slot = floor(pos);
    float x = uv.x + (pos - slot) * SLOT_PITCH;
    if (x > 0.5 * SLOT_PITCH) {
        x -= SLOT_PITCH;
        slot = mod(slot + 1.0, slots);
    }
    vec2 p = vec2(x, uv.y);
    if (dot(p, p) > 1.1025) return 0.0;  // slot hole slightly larger than the aperture
    if (!gdtf && slot < 0.5) return 1.0;
    float layer = gdtf ? range.x + slot : slot - 1.0;
    vec2 g = rot2(wheel.y) * p;
    return textureLod(u_gobos, vec3(g * 0.5 + 0.5, layer), u_lod_base + wheel.z).r;
}

void main() {
    o_light = vec4(goboWheel(v_uv, v_wheel1, v_range1) * goboWheel(v_uv, v_wheel2, v_range2));
}
