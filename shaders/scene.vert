in vec3 in_pos;
in vec3 in_normal;
in vec3 in_albedo;

uniform mat4 u_view_proj;

out vec3 v_normal;
out vec3 v_albedo;
out vec3 v_emissive;

void main() {
    v_normal = in_normal;
    v_albedo = in_albedo;
    v_emissive = vec3(0.0);
    gl_Position = u_view_proj * vec4(in_pos, 1.0);
}
