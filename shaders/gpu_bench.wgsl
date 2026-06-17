@group(0) @binding(0)
var<storage, read_write> data: array<u32, 1024>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx < 1024u {
        var acc: u32 = 0u;
        for (var i = 0u; i < 1024u; i++) {
            acc += i * 3u + 7u;
        }
        data[idx] = acc;
    }
}
