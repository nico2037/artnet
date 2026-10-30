in vec3 v_world;

uniform vec3 u_origin;

out vec4 o_distance;

void main() {
    o_distance = vec4(distance(v_world, u_origin));
}
