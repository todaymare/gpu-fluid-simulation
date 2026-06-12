struct Vertex {
    @location(0) position: vec2<f32>,
}


struct Instance {
    @location(1) colour: vec4<f32>,
    @location(2) uv: vec4<f32>,
    @location(3) pos: vec2<f32>,
    @location(4) scale: vec2<f32>,
    @location(5) rot: f32,
    @location(6) z: f32,
    @location(7) kind: u32,
}


struct Fragment {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) abs_uv: vec2<f32>,
    @location(2) modulate: vec4<f32>,
    @location(3) @interpolate(flat) kind: u32,
    @location(4) world_pos: vec2<f32>,
}


struct Uniforms {
    proj: mat4x4<f32>,
    pad: vec3<u32>,
    treshold: f32,
}

@group(0) @binding(0) var<uniform> u : Uniforms;

@group(1) @binding(0)
var tex: texture_2d<f32>;
@group(1) @binding(1)
var samp: sampler;


fn make_transform_2d_mat4(pos: vec3<f32>, scale: vec2<f32>, rot: f32) -> mat4x4<f32> {
    let c = cos(rot);
    let s = sin(rot);

    return mat4x4<f32>(
        scale.x * c , scale.x * s, 0.0, 0.0,
        -scale.y * s, scale.y * c, 0.0, 0.0,
        0.0,          0.0        , 1.0, 0.0,
        pos.x,        pos.y      , pos.z, 1.0
    );
}


@vertex
fn vs_main(vertex: Vertex, instance: Instance) -> Fragment {
    var output : Fragment;

    let mat = make_transform_2d_mat4(vec3(instance.pos, instance.z), instance.scale, instance.rot);


    output.position = u.proj * mat * vec4(vertex.position, 0.0, 1.0);
    output.modulate = instance.colour;

    let local = vertex.position + vec2<f32>(0.5, 0.5);
    let uv0 = instance.uv.xy;
    let uv1 = instance.uv.zw;
    // abs_uv is 0..1 across the sprite instead of across the whole texture, so it can be used for gradients and such
    output.abs_uv = local;

    output.uv = uv0 + local * (uv1 - uv0);
    output.kind = instance.kind;
    output.world_pos = instance.pos + vertex.position * instance.scale;

    return output;
}


fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}


@fragment
fn fs_main(fragment: Fragment) -> @location(0) vec4<u32> {
    let tex_color = textureSample(tex, samp, fragment.uv);
    var colour = tex_color * fragment.modulate;
    var lum = luminance(colour.xyz) * colour.w;

    if lum > u.treshold { return vec4(255u); }
    else { return vec4(0u); }
}

