uniform sampler2D u_depth;
uniform mat4 u_inv_view_proj;
uniform vec3 u_cam_pos;

// Light image atlas: one tile per fixture with all wheels in front of the lens already composed.
uniform sampler2D u_light_image;
uniform vec2 u_light_grid;       // tiles per row, tile size in px
uniform vec2 u_light_size;       // atlas size in px

// Shadow atlas: per fixture 6 cube-face tiles storing distance to the nearest static occluder.
uniform sampler2D u_shadow;
uniform mat3 u_shadow_faces[6];  // world dir -> face view space, order +x -x +y -y +z -z
uniform vec2 u_shadow_grid;      // tiles per row, tile size in px
uniform vec2 u_shadow_size;      // atlas size in px

float shadow(vec3 p, vec4 info) {
    vec3 d = p - info.xyz;
    float dist = length(d);
    vec3 a = abs(d);
    int face = (a.x >= a.y && a.x >= a.z) ? (d.x > 0.0 ? 0 : 1)
             : (a.y >= a.z) ? (d.y > 0.0 ? 2 : 3)
             : (d.z > 0.0 ? 4 : 5);
    vec3 v = u_shadow_faces[face] * d;
    vec2 uv = clamp(v.xy / -v.z, -1.0, 1.0) * 0.5 + 0.5;
    float tile = info.w * 6.0 + float(face);
    vec2 cell = vec2(mod(tile, u_shadow_grid.x), floor(tile / u_shadow_grid.x));
    float size = u_shadow_grid.y;
    vec2 px = cell * size + 0.5 + uv * (size - 1.0);  // half-texel inset: no bleeding between tiles
    float occluder = texture(u_shadow, px / u_shadow_size).r;
    return dist <= occluder + 0.03 + 0.01 * dist ? 1.0 : 0.0;
}

vec3 worldFromDepth(vec2 uv, float depth) {
    vec4 p = u_inv_view_proj * vec4(vec3(uv, depth) * 2.0 - 1.0, 1.0);
    return p.xyz / p.w;
}

float ign(vec2 p) {
    return fract(52.9829189 * fract(dot(p, vec2(0.06711056, 0.00583715))));
}

// COLOR_IMAGE: the light image is RGB (colour wheel split); otherwise one transmission channel.
#ifdef COLOR_IMAGE
#define LIGHT vec3
#define LIGHT_CHANNELS(c) (c).rgb
#define LIGHT_PEAK(m) max((m).r, max((m).g, (m).b))
#else
#define LIGHT float
#define LIGHT_CHANNELS(c) (c).r
#define LIGHT_PEAK(m) (m)
#endif

LIGHT lightImage(vec2 uv, float slot) {
    vec2 cell = vec2(mod(slot, u_light_grid.x), floor(slot / u_light_grid.x)) * u_light_grid.y;
    vec2 px = clamp(cell + (uv * 0.5 + 0.5) * u_light_grid.y, cell + 0.5, cell + u_light_grid.y - 0.5);
    return LIGHT_CHANNELS(texture(u_light_image, px / u_light_size));
}

// l: point in beam space (apex at origin, beam along +z). Returns transmitted fraction 0..1.
LIGHT beamMask(vec3 l, float tanHalf, float focus, float slot) {
    if (l.z <= 0.0) return LIGHT(0.0);
    vec2 uv = l.xy / (l.z * tanHalf);
    float soft = mix(0.03, 0.35, focus);
    float m = 1.0 - smoothstep(1.0 - soft, 1.0, length(uv));
    if (m <= 0.0) return LIGHT(0.0);
    return m * lightImage(uv, slot);
}

// Ray o + t*d (beam space) against solid cone x^2+y^2 <= (k z)^2, 0 <= z <= L. Miss if result.x >= result.y.
vec2 coneInterval(vec3 o, vec3 d, float k, float L) {
    float t0 = -1e9, t1 = 1e9;
    if (abs(d.z) > 1e-6) {
        float ta = -o.z / d.z, tb = (L - o.z) / d.z;
        t0 = min(ta, tb);
        t1 = max(ta, tb);
    } else if (o.z < 0.0 || o.z > L) {
        return vec2(1.0, 0.0);
    }

    float k2 = k * k;
    float a = d.x * d.x + d.y * d.y - k2 * d.z * d.z;
    float b = 2.0 * (o.x * d.x + o.y * d.y - k2 * o.z * d.z);
    float c = o.x * o.x + o.y * o.y - k2 * o.z * o.z;

    if (abs(a) < 1e-7) {
        if (abs(b) < 1e-9) return c > 0.0 ? vec2(1.0, 0.0) : vec2(t0, t1);
        float tl = -c / b;
        return b > 0.0 ? vec2(t0, min(t1, tl)) : vec2(max(t0, tl), t1);
    }
    float disc = b * b - 4.0 * a * c;
    if (disc < 0.0) return a > 0.0 ? vec2(1.0, 0.0) : vec2(t0, t1);

    float sq = sqrt(disc);
    float r0 = (-b - sq) / (2.0 * a);
    float r1 = (-b + sq) / (2.0 * a);
    if (r0 > r1) { float tmp = r0; r0 = r1; r1 = tmp; }
    if (a > 0.0) return vec2(max(t0, r0), min(t1, r1));

    // a < 0: inside for t <= r0 or t >= r1; only one part lies in the z >= 0 nappe
    vec2 A = vec2(t0, min(t1, r0));
    vec2 B = vec2(max(t0, r1), t1);
    return (A.y - A.x) > (B.y - B.x) ? A : B;
}
