in float i_slot;
in vec3 i_wheel1;   // wheel position, rotation (rad), defocus blur (mip levels)
in vec3 i_wheel2;

uniform vec2 u_light_grid;   // tiles per row, tile size in px
uniform vec2 u_light_size;   // atlas size in px

out vec2 v_uv;
flat out vec3 v_wheel1;
flat out vec3 v_wheel2;

void main() {
    vec2 corner = vec2(gl_VertexID & 1, gl_VertexID >> 1);
    vec2 cell = vec2(mod(i_slot, u_light_grid.x), floor(i_slot / u_light_grid.x));
    vec2 px = (cell + corner) * u_light_grid.y;
    v_uv = corner * 2.0 - 1.0;
    v_wheel1 = i_wheel1;
    v_wheel2 = i_wheel2;
    gl_Position = vec4(px / u_light_size * 2.0 - 1.0, 0.0, 1.0);
}
