
@group(0) @binding(0) var<storage, read_write> returned: vec4<f32>;
@group(0) @binding(1) var<storage, read> param: vec4<f32>;

@compute
@workgroup_size(1)
fn main() {
    returned = param * 2.0;
}