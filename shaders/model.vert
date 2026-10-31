// GDTF fixture models: one instance per (fixture, part) with the part's world transform.
in vec3 in_pos;
in vec3 in_normal;

in vec3 i_c0, i_c1, i_c2;   // linear part of the world transform (may scale)
in vec3 i_t;
in vec3 i_emissive;        // lens glow for the beam part, 0 elsewhere

uniform mat4 u_view_proj;

out vec3 v_normal;
out vec3 v_albedo;
out vec3 v_emissive;

void main() {
    mat3 M = mat3(i_c0, i_c1, i_c2);
    v_normal = normalize(transpose(inverse(M)) * in_normal);
    bool lens = dot(i_emissive, i_emissive) > 0.0;
    v_albedo = lens ? vec3(0.0) : vec3(0.04);
    v_emissive = i_emissive;
    gl_Position = u_view_proj * vec4(i_t + M * in_pos, 1.0);
}
