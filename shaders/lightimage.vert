in float i_slot;
in vec3 i_wheel1;   // wheel position, rotation (rad), defocus blur (mip levels)
in vec3 i_wheel2;
in vec2 i_range1;   // first gobo layer of the wheel, slot count; count 0 = whole library (demo)
in vec2 i_range2;
in float i_iris;
in vec3 i_color1;   // colour wheel split (COLOR_IMAGE only): current slot, next slot, scroll fraction
in vec3 i_color2;
in float i_split;    // closed fraction of the iris, 0 = open (also when the attribute is not supplied)

uniform vec2 u_light_grid;   // tiles per row, tile size in px
uniform vec2 u_light_size;   // atlas size in px

out vec2 v_uv;
flat out vec3 v_wheel1;
flat out vec3 v_wheel2;
flat out vec2 v_range1;
flat out vec2 v_range2;
flat out float v_aperture;
flat out vec3 v_color1;
flat out vec3 v_color2;
flat out float v_split;

void main() {
    vec2 corner = vec2(gl_VertexID & 1, gl_VertexID >> 1);
    vec2 cell = vec2(mod(i_slot, u_light_grid.x), floor(i_slot / u_light_grid.x));
    vec2 px = (cell + corner) * u_light_grid.y;
    v_uv = corner * 2.0 - 1.0;
    v_wheel1 = i_wheel1;
    v_wheel2 = i_wheel2;
    v_range1 = i_range1;
    v_range2 = i_range2;
    v_aperture = 1.0 - clamp(i_iris, 0.0, 0.95);
    v_color1 = i_color1;
    v_color2 = i_color2;
    v_split = i_split;
    gl_Position = vec4(px / u_light_size * 2.0 - 1.0, 0.0, 1.0);
}
