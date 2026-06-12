#include shaders/funcs.wgsl

struct Vertex {
    @location(0) position: vec2<f32>,
}


@group(2) @binding(0) var<uniform> inv_proj : mat4x4<f32>;


struct Fragment {
    @builtin(position) screen_position: vec4<f32>,
    @location(0) clip_position: vec4<f32>,
}



@vertex
fn vs_main(vertex: Vertex) -> Fragment {
    var output : Fragment;
    output.screen_position = vec4((vertex.position-vec2(0.5)) * 2.0, 0.0, 1.0);
    output.clip_position = output.screen_position;
    return output;
}


@fragment
fn fs_main(input: Fragment) -> @location(0) vec4<f32> {
    let clip_pos = input.clip_position;
    let inv_pos = inv_proj * clip_pos;
    let world_pos = (inv_pos.xyz / inv_pos.w).xy;
    let point = world_pos;

    var density: f32 = 0.0;
    var velocity_factor: f32 = 0.0;


    let cell = vec2<i32>(xy_of_point(point));
    for (var offset_y = -2; offset_y < 3; offset_y = offset_y + 1) {
        for (var offset_x = -2; offset_x < 3; offset_x = offset_x + 1) {
            let x = u32(cell.x + offset_x);
            let y = u32(cell.y + offset_y);

            let id = grid_pos_to_id(vec2<u32>(x, y));
            var start_index = start_indices[id];


            while true {
                if start_index >= u.particle_count { break; }

                let neighbour = in_particles[start_index];

                if neighbour.grid != id { break; }

                let i = start_index;
                start_index += 1;

                // func start


                let p = in_particles[i].predicted_position;
                let vel = in_particles[i].velocity;
                let offset = p - point;
                let r2 = dot(offset, offset);

                let contrib = exp(-r2 / (0.06));
                density += contrib;
                velocity_factor += contrib * length(vel); // weighted by proximity
                // func end


            }

        }
    }


    // Normalize / scale velocity factor for color mapping
    velocity_factor = velocity_factor * 0.0055;

    let log_factor = 5.0; 
    velocity_factor = log(1.0 + log_factor * velocity_factor) / log(1.0 + log_factor);
    velocity_factor = clamp(velocity_factor, 0.0, 1.0);

    // Fluid interior
    let interior = smoothstep(0.5, 1.5, density);

    // Edge highlighting
    var edge = smoothstep(0.7, 1.0, density) - smoothstep(1.0, 1.5, density);
    edge = edge * (1.0 + velocity_factor * 2.0); // moving particles = stronger edges

    // Color mapping: blue (slow) → red (fast)
    let base_color = mix(vec3<f32>(0.0, 0.5, 1.0), vec3<f32>(1.0, 0.0, 0.0), velocity_factor) * interior;
    let edge_color = vec3<f32>(1.0, 1.0, 1.0) * edge;

    let final_color = base_color + edge_color;

    // Alpha
    let alpha = clamp(interior, 0.0, 1.0);

    if density > 50.0 {
        return vec4<f32>(0.0, 0.0, 1.0, 1.0);
    }

    if u.show_force_field != 0u {
        let uv = (world_pos / u.bounds) + vec2<f32>(0.5);
        let pos_f = uv * u.texture_size - vec2<f32>(0.5);
        let pos = vec2<i32>(floor(pos_f));
        let fx = pos_f.x - f32(pos.x);
        let fy = pos_f.y - f32(pos.y);

        let w_x = i32(u.texture_size.x);
        let w_y = i32(u.texture_size.y);
        let xc0 = clamp(pos.x, 0, w_x - 1);
        let yc0 = clamp(pos.y, 0, w_y - 1);
        let xc1 = clamp(pos.x + 1, 0, w_x - 1);
        let yc1 = clamp(pos.y + 1, 0, w_y - 1);

        let f00 = force_field[u32(yc0) * u32(w_x) + u32(xc0)];
        let f10 = force_field[u32(yc0) * u32(w_x) + u32(xc1)];
        let f01 = force_field[u32(yc1) * u32(w_x) + u32(xc0)];
        let f11 = force_field[u32(yc1) * u32(w_x) + u32(xc1)];
        let grad = mix(mix(f00, f10, fx), mix(f01, f11, fx), fy);

        // Encode [-1, 1] -> [0, 1] per channel. R = x, G = y, B = 0.
        let visual = vec3<f32>(grad.x * 0.5 + 0.5, grad.y * 0.5 + 0.5, 0.0);
        return vec4<f32>(visual, 1.0);
    }

    return vec4<f32>(final_color, alpha);

}
