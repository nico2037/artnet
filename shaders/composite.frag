in vec2 v_uv;

uniform sampler2D u_albedo;
uniform sampler2D u_light;
uniform sampler2D u_volume;
uniform vec2 u_volume_uv_scale;  // used part of the volume buffer (dynamic resolution)
uniform vec2 u_volume_uv_max;    // half a texel inside that part, avoids stale texels
uniform float u_ambient;

out vec4 o_color;

vec3 aces(vec3 x) {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), 0.0, 1.0);
}

void main() {
    vec3 hdr = texture(u_albedo, v_uv).rgb * u_ambient
             + texture(u_light, v_uv).rgb
             + texture(u_volume, min(v_uv * u_volume_uv_scale, u_volume_uv_max)).rgb;
    vec3 c = pow(aces(hdr), vec3(1.0 / 2.2));
    float dither = fract(52.9829189 * fract(dot(gl_FragCoord.xy, vec2(0.06711056, 0.00583715))));
    o_color = vec4(c + (dither - 0.5) / 255.0, 1.0);
}
