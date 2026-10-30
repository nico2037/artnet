in vec3 in_pos;
in vec3 in_normal;
in float in_part;

in vec3 i_pos;
in vec3 i_m0, i_m1, i_m2;
in vec3 i_p0, i_p1, i_p2;
in vec3 i_b0, i_b1, i_b2;
in vec3 i_lens;

uniform mat4 u_view_proj;

out vec3 v_normal;
out vec3 v_albedo;
out vec3 v_emissive;

void main() {
    // part: 0 base (mount frame), 1 yoke (pan frame), 2 head / 3 lens (beam frame)
    mat3 R = in_part < 0.5 ? mat3(i_m0, i_m1, i_m2)
           : in_part < 1.5 ? mat3(i_p0, i_p1, i_p2)
           : mat3(i_b0, i_b1, i_b2);
    bool lens = in_part > 2.5;
    v_normal = R * in_normal;
    v_albedo = lens ? vec3(0.0) : vec3(0.04);
    v_emissive = lens ? i_lens * 25.0 : vec3(0.0);
    gl_Position = u_view_proj * vec4(i_pos + R * in_pos, 1.0);
}
