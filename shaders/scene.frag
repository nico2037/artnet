in vec3 v_normal;
in vec3 v_albedo;
in vec3 v_emissive;

layout(location = 0) out vec4 o_albedo;
layout(location = 1) out vec4 o_normal;
layout(location = 2) out vec4 o_light;

void main() {
    o_albedo = vec4(v_albedo, 1.0);
    o_normal = vec4(normalize(v_normal), 1.0);
    o_light = vec4(v_emissive, 1.0);
}
