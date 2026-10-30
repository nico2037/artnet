in vec3 v_world;
flat in vec3 v_apex;
flat in mat3 v_basis;
flat in vec3 v_cone;
flat in vec3 v_color;
flat in vec4 v_shadow;

uniform vec2 u_target_size;
uniform float u_gain;

out vec4 o_color;

#ifdef SURFACE
uniform sampler2D u_albedo;
uniform sampler2D u_normal;

void main() {
    vec2 uv = gl_FragCoord.xy / u_target_size;
    float depth = texture(u_depth, uv).r;
    if (depth >= 1.0) discard;

    vec3 P = worldFromDepth(uv, depth);
    vec3 l = transpose(v_basis) * (P - v_apex);
    if (l.z > v_cone.y) discard;
    float m = beamMask(l, v_cone.x, v_cone.z, v_shadow.w);
    if (m <= 0.0) discard;

    vec3 N = texture(u_normal, uv).xyz;
    vec3 toLight = v_apex - P;
    float d2 = dot(toLight, toLight);
    m *= shadow(P + N * (0.02 + 0.01 * sqrt(d2)), v_shadow);
    if (m <= 0.0) discard;
    float ndl = max(dot(N, toLight * inversesqrt(d2)), 0.0);
    vec3 albedo = texture(u_albedo, uv).rgb;
    o_color = vec4(v_color * albedo * (m * ndl * u_gain / d2), 1.0);
}
#endif

#ifdef VOLUME
uniform sampler3D u_noise;
uniform int u_steps;
uniform float u_noise_amount;
uniform float u_time;
uniform float u_haze;
uniform float u_extinction;
uniform float u_g;

float hazeDensity(vec3 p) {
    if (u_noise_amount <= 0.0) return 1.0;
    vec3 wind = vec3(0.12, 0.02, 0.07) * u_time;
    float n = texture(u_noise, p * 0.08 + wind).r * 0.65 + texture(u_noise, p * 0.21 - wind * 1.7).r * 0.35;
    return mix(1.0, 2.0 * smoothstep(0.2, 0.8, n), u_noise_amount);
}

float henyeyGreenstein(float cosTheta, float g) {
    float g2 = g * g;
    return (1.0 - g2) / pow(1.0 + g2 - 2.0 * g * cosTheta, 1.5);
}

void main() {
    vec2 uv = gl_FragCoord.xy / u_target_size;
    float depth = texture(u_depth, uv).r;
    vec3 rd = normalize(v_world - u_cam_pos);
    float tScene = depth >= 1.0 ? 1e4 : length(worldFromDepth(uv, depth) - u_cam_pos);

    mat3 Bt = transpose(v_basis);
    vec3 o = Bt * (u_cam_pos - v_apex);
    vec3 d = Bt * rd;
    vec2 seg = coneInterval(o, d, v_cone.x, v_cone.y);
    seg.x = max(seg.x, 0.0);
    seg.y = min(seg.y, tScene);
    if (seg.y <= seg.x) discard;

    float dt = (seg.y - seg.x) / float(u_steps);
    float jitter = ign(gl_FragCoord.xy);
    float sum = 0.0;
    for (int i = 0; i < u_steps; ++i) {
        float t = seg.x + (float(i) + jitter) * dt;
        vec3 l = o + d * t;
        float m = beamMask(l, v_cone.x, v_cone.z, v_shadow.w);
        if (m <= 0.0) continue;
        vec3 p = u_cam_pos + rd * t;
        m *= shadow(p, v_shadow);
        if (m <= 0.0) continue;
        float dist2 = dot(l, l);
        float phase = henyeyGreenstein(dot(l, -d) * inversesqrt(dist2), u_g);
        sum += m * phase * hazeDensity(p) / (dist2 + 0.3);
    }
    float transmittance = exp(-u_extinction * seg.x);
    o_color = vec4(v_color * (sum * dt * u_haze * u_gain * transmittance), 1.0);
}
#endif
