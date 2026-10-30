in vec3 in_vert;

in vec3 i_apex;
in vec3 i_ax, i_ay, i_az;
in vec3 i_cone;   // tan(half angle), length, focus
in vec3 i_color;
in vec4 i_shadow; // shadow origin (head center), fixture slot

uniform mat4 u_view_proj;
uniform float u_mesh_scale;

out vec3 v_world;
flat out vec3 v_apex;
flat out mat3 v_basis;
flat out vec3 v_cone;
flat out vec3 v_color;
flat out vec4 v_shadow;

void main() {
    mat3 B = mat3(i_ax, i_ay, i_az);
    float len = i_cone.y;
    float radius = i_cone.x * len * u_mesh_scale;
    vec3 world = i_apex + B * vec3(in_vert.xy * radius, in_vert.z * len);

    v_world = world;
    v_apex = i_apex;
    v_basis = B;
    v_cone = i_cone;
    v_color = i_color;
    v_shadow = i_shadow;
    gl_Position = u_view_proj * vec4(world, 1.0);
}
